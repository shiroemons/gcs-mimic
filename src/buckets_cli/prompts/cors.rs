//! The CORS questions. Browser uploads and downloads give a rule that browsers can use from a
//! choice of origins alone; methods, response headers and max age are asked only for a custom
//! rule, starting from what those goals need.

use super::lists::{Offer, Pick, SecondsPick, custom, pick_many, pick_seconds, positions};
use super::terminal::{EnterMeans, Entry, Need, Question, Terminal};
use gcs_mimic::bucket_init::generate::{
    self, Bucket, CorsMethod, CorsOrigin, CorsResponseHeader, Setting,
};
use inquire::InquireError;

pub(super) const METHODS: [&str; 6] = ["GET", "HEAD", "PUT", "POST", "DELETE", "OPTIONS"];

/// The default origins of common local development servers and of the web views that hybrid
/// mobile apps load their pages in. A browser sends the origin exactly as its address bar would
/// show it, so other hosts, ports and schemes are typed.
pub(super) const ORIGINS: [Offer; 8] = [
    Offer {
        value: "http://localhost:3000",
        note: "Next.js, Rails, Nuxt",
    },
    Offer {
        value: "http://localhost:5173",
        note: "Vite, SvelteKit",
    },
    Offer {
        value: "http://localhost:4200",
        note: "Angular",
    },
    Offer {
        value: "http://localhost:8080",
        note: "Vue CLI, webpack-dev-server",
    },
    Offer {
        value: "http://localhost:8000",
        note: "Django, Laravel",
    },
    Offer {
        value: "https://localhost",
        note: "Capacitor on Android, HTTPS on port 443",
    },
    Offer {
        value: "capacitor://localhost",
        note: "Capacitor on iOS",
    },
    Offer {
        value: "*",
        note: "any origin",
    },
];

/// The origin whose port the origin list asks for alone. Local development servers move to the
/// next free port, such as 3001 or 5174, when theirs is taken, and a port is quicker to type and
/// harder to get wrong than a whole origin; listing every such port would bury the common ones.
pub(super) const LOCALHOST: &str = "http://localhost";

/// The request headers browsers send to a bucket, which a preflight refuses unless the rule
/// lists them, and the response header scripts read.
pub(super) const RESPONSE_HEADERS: [Offer; 4] = [
    Offer {
        value: "Content-Type",
        note: "browser uploads send it",
    },
    Offer {
        value: "x-goog-resumable",
        note: "resumable uploads from a browser",
    },
    Offer {
        value: "ETag",
        note: "lets scripts read an object's ETag",
    },
    Offer {
        value: "*",
        note: "any header",
    },
];

pub(super) const MAX_AGES: [SecondsPick; 4] = [
    SecondsPick::Offered {
        seconds: 3600,
        means: "1 hour",
    },
    SecondsPick::Offered {
        seconds: 86_400,
        means: "1 day",
    },
    SecondsPick::Unset("None: each browser keeps its own limit"),
    SecondsPick::Other,
];

/// What the bucket's CORS rule lets a frontend do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Access {
    pub(super) uploads: bool,
    pub(super) downloads: bool,
    /// Methods, response headers and max age are asked, starting from what the others need.
    pub(super) custom: bool,
}

impl Access {
    pub(super) fn wanted(self) -> bool {
        self.uploads || self.downloads || self.custom
    }
}

/// The methods and request headers a browser needs for an [`Access`], which a custom rule starts
/// from. They are the ones the browser goals add, as their flags add them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Needs {
    pub(super) methods: Vec<&'static str>,
    pub(super) headers: Vec<&'static str>,
}

impl From<Access> for Needs {
    fn from(access: Access) -> Self {
        let downloads = access.downloads.then_some(generate::DOWNLOAD_METHODS);
        let uploads = access.uploads.then_some(generate::UPLOAD_METHODS);
        let headers = access.uploads.then_some(generate::UPLOAD_HEADERS);
        Self {
            methods: downloads.into_iter().chain(uploads).flatten().collect(),
            headers: headers.into_iter().flatten().collect(),
        }
    }
}

/// Asks for the bucket's CORS rule, records it and returns its origins. `before` holds origins
/// given earlier, which the bucket usually shares: its own when it is asked again, otherwise the
/// latest bucket's.
pub(super) fn ask(
    term: &mut impl Terminal,
    bucket: &mut Bucket,
    access: Access,
    before: &[CorsOrigin],
) -> Result<Vec<CorsOrigin>, InquireError> {
    let origins = origins(term, before)?;
    let rule: Vec<Setting> = if access.custom {
        custom_rule(term, &Needs::from(access))?
            .into_settings()
            .collect()
    } else {
        // A long max age would let a browser keep an old rule after the document changes, and a
        // preflight to a local server costs little, so the goals set none.
        let downloads = access.downloads.then_some(Setting::BrowserDownloads);
        let uploads = access.uploads.then_some(Setting::BrowserUploads);
        downloads.into_iter().chain(uploads).collect()
    };
    let settings = origins.iter().cloned().map(Setting::CorsOrigin).chain(rule);
    for setting in settings {
        bucket.apply(setting).map_err(custom)?;
    }
    Ok(origins)
}

/// What a CORS rule allows from its origins.
struct Rule {
    methods: Vec<CorsMethod>,
    headers: Vec<CorsResponseHeader>,
    max_age: Option<u64>,
}

impl Rule {
    fn into_settings(self) -> impl Iterator<Item = Setting> {
        self.methods
            .into_iter()
            .map(Setting::CorsMethod)
            .chain(self.headers.into_iter().map(Setting::CorsResponseHeader))
            .chain(self.max_age.map(Setting::CorsMaxAgeSeconds))
    }
}

fn origins(
    term: &mut impl Terminal,
    before: &[CorsOrigin],
) -> Result<Vec<CorsOrigin>, InquireError> {
    if !before.is_empty() {
        let listed: Vec<_> = before.iter().map(ToString::to_string).collect();
        let question = format!("Same CORS origins as before ({})?", listed.join(", "));
        if term.confirm(&question, EnterMeans::Yes)? {
            return Ok(before.to_vec());
        }
    }
    pick_many(
        term,
        "CORS origins:",
        &ORIGINS,
        &[
            Pick::Port(LOCALHOST),
            Pick::Other("Another origin, such as https://app.example.test"),
        ],
        Need::AtLeastOne,
        &[],
    )
}

/// Asks for the methods, response headers and max age, with what the goals need marked. A rule
/// needs a method, so the method question needs an answer.
fn custom_rule(term: &mut impl Terminal, needs: &Needs) -> Result<Rule, InquireError> {
    let shown: Vec<_> = METHODS.iter().map(Entry::plain).collect();
    let methods: Vec<&str> = term
        .select_many(
            Question::new("CORS methods:"),
            &shown,
            &positions(&METHODS, &needs.methods),
            Need::AtLeastOne,
        )?
        .into_iter()
        .filter_map(|index| METHODS.get(index).copied())
        .collect();
    // A browser upload of a file sends Content-Type, whose preflight fails unless it is listed.
    let uploads = methods
        .iter()
        .any(|method| matches!(*method, "PUT" | "POST"));
    let mut wanted = needs.headers.clone();
    if uploads && !wanted.contains(&"Content-Type") {
        wanted.push("Content-Type");
    }
    let offered: Vec<_> = RESPONSE_HEADERS.iter().map(|offer| offer.value).collect();
    let headers = pick_many(
        term,
        "CORS response headers:",
        &RESPONSE_HEADERS,
        &[Pick::Other("Another header")],
        Need::Optional,
        &positions(&offered, &wanted),
    )?;
    let max_age = pick_seconds(
        term,
        "CORS max age:",
        "CORS max age in seconds:",
        &MAX_AGES,
        u64::from(generate::MAX_CORS_MAX_AGE_SECONDS),
    )?;
    Ok(Rule {
        methods: parsed(&methods)?,
        headers,
        max_age,
    })
}

fn parsed<T>(values: &[&str]) -> Result<Vec<T>, InquireError>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    values
        .iter()
        .map(|value| value.parse().map_err(custom))
        .collect()
}
