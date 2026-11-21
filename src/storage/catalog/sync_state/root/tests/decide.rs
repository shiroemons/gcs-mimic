//! Every combination of what startup can read, and what it decides for each.
//!
//! The decision reads only the selected directory's location and identity, the marker it holds,
//! the registered roots and the activation the operator asked for, so each table below states one
//! activation's decision for every layout of registered roots and every marker, and
//! `decides_every_combination_once` fails when a combination is missing or listed twice.

use super::super::Marker;
use super::super::{
    Activation, RootError, RootId,
    decide::{Decision, decide},
    facts::{CatalogFacts, DiskFacts},
    switch::{Reason, Registered},
};
use crate::{fsutil::NodeId, storage::sync_state::CatalogId};

const SELECTED: &[u8] = b"/sync/selected";
const OTHER: &[u8] = b"/sync/other";

fn node(inode: u64) -> NodeId {
    NodeId::from_be_bytes(1u64.to_be_bytes(), inode.to_be_bytes())
}
fn root(byte: u8) -> RootId {
    RootId([byte; 16])
}
fn catalog(byte: u8) -> CatalogId {
    CatalogId::from_bytes([byte; 16])
}
const THIS: u8 = 1;
const ANOTHER: u8 = 2;

/// The root the selected directory belongs to when it is registered.
const HERE: u8 = 0xA1;
/// A registered root somewhere else.
const ELSEWHERE: u8 = 0xB2;
const SECOND_ELSEWHERE: u8 = 0xC3;
/// A root the catalog never registered.
const UNREGISTERED: u8 = 0xEE;

fn registered(byte: u8, location: &[u8], inode: u64, complete: bool) -> Registered {
    Registered {
        root_id: root(byte),
        location: location.to_vec(),
        node: node(inode),
        complete,
    }
}

/// What the catalog registers, by how it relates to the selected directory (path `SELECTED`,
/// inode 100).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    Empty,
    /// One root at the selected path and inode, its baseline built.
    SoleHere,
    /// The same, its baseline still to build.
    SoleHereUnbuilt,
    /// One root at the selected path but another inode.
    SoleSamePath,
    /// One root at another path but the selected inode.
    SoleSameDirectory,
    SoleElsewhere,
    /// Two roots, one at the selected path and inode.
    TwoOneHere,
    TwoElsewhere,
}

const LAYOUTS: [Layout; 8] = [
    Layout::Empty,
    Layout::SoleHere,
    Layout::SoleHereUnbuilt,
    Layout::SoleSamePath,
    Layout::SoleSameDirectory,
    Layout::SoleElsewhere,
    Layout::TwoOneHere,
    Layout::TwoElsewhere,
];

impl Layout {
    fn roots(self) -> Vec<Registered> {
        let here = |complete| registered(HERE, SELECTED, 100, complete);
        match self {
            Self::Empty => vec![],
            Self::SoleHere => vec![here(true)],
            Self::SoleHereUnbuilt => vec![here(false)],
            Self::SoleSamePath => vec![registered(HERE, SELECTED, 200, true)],
            Self::SoleSameDirectory => vec![registered(HERE, OTHER, 100, true)],
            Self::SoleElsewhere => vec![registered(HERE, OTHER, 200, true)],
            Self::TwoOneHere => vec![here(true), registered(ELSEWHERE, OTHER, 300, true)],
            Self::TwoElsewhere => vec![
                registered(ELSEWHERE, OTHER, 300, true),
                registered(SECOND_ELSEWHERE, b"/sync/third", 400, true),
            ],
        }
    }

    /// The root the selected directory's marker names when it names the selected directory's own.
    fn here(self) -> Option<u8> {
        matches!(
            self,
            Self::SoleHere | Self::SoleHereUnbuilt | Self::TwoOneHere
        )
        .then_some(HERE)
    }

    /// A registered root that is not the selected directory's.
    fn elsewhere(self) -> Option<u8> {
        match self {
            Self::Empty | Self::SoleHere | Self::SoleHereUnbuilt => None,
            Self::SoleSamePath | Self::SoleSameDirectory | Self::SoleElsewhere => Some(HERE),
            Self::TwoOneHere | Self::TwoElsewhere => Some(ELSEWHERE),
        }
    }

    fn baseline_built(self) -> bool {
        self != Self::SoleHereUnbuilt
    }
}

/// The marker the selected directory holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Found {
    Nothing,
    /// A marker of another data directory's catalog.
    AnotherCatalogs,
    /// This catalog's, naming the selected directory's own root.
    OwnHere,
    /// This catalog's, naming a registered root that is not the selected directory's.
    OwnElsewhere,
    /// This catalog's, naming a root it never registered.
    OwnUnregistered,
}

const FOUND: [Found; 5] = [
    Found::Nothing,
    Found::AnotherCatalogs,
    Found::OwnHere,
    Found::OwnElsewhere,
    Found::OwnUnregistered,
];

impl Found {
    /// Whether the layout has a root of the kind this marker names.
    fn exists_in(self, layout: Layout) -> bool {
        match self {
            Self::Nothing | Self::AnotherCatalogs | Self::OwnUnregistered => true,
            Self::OwnHere => layout.here().is_some(),
            Self::OwnElsewhere => layout.elsewhere().is_some(),
        }
    }

    fn marker(self, layout: Layout) -> Option<Marker> {
        let (catalog_id, root_id) = match self {
            Self::Nothing => return None,
            Self::AnotherCatalogs => (catalog(ANOTHER), root(UNREGISTERED)),
            Self::OwnHere => (catalog(THIS), root(layout.here()?)),
            Self::OwnElsewhere => (catalog(THIS), root(layout.elsewhere()?)),
            Self::OwnUnregistered => (catalog(THIS), root(UNREGISTERED)),
        };
        Some(Marker {
            version: 1,
            catalog_id,
            root_id,
        })
    }
}

/// How the operator asked startup to take the directory.
#[derive(Clone, Copy, Debug)]
enum Ask {
    Open,
    ReinitializeThisCatalog,
    ReinitializeAnotherCatalog,
}

impl Ask {
    fn activation(self) -> Activation {
        match self {
            Self::Open => Activation::Open,
            Self::ReinitializeThisCatalog => Activation::Reinitialize {
                catalog_id: catalog(THIS),
            },
            Self::ReinitializeAnotherCatalog => Activation::Reinitialize {
                catalog_id: catalog(ANOTHER),
            },
        }
    }
}

/// What a row of a table expects, with the roots it names resolved per layout.
#[derive(Clone, Copy, Debug)]
enum Expect {
    /// Opens the marker's root, with the baseline the layout registered.
    Open,
    /// Builds the registered root's baseline again.
    Reinitialize,
    First,
    Replace,
    /// Rebinds under the selected directory's own registered root because several are registered.
    RebindBecauseSeveral,
    /// Rebinds under the root the marker names.
    RebindMarked,
    ForeignMarker,
    MissingMarker,
    RootChanged,
    ReinitializeDenied,
}

impl Expect {
    fn resolve(self, layout: Layout, marker: Option<Marker>) -> Result<Decision, RootError> {
        let marked = || marker.expect("the row expects a marker").root_id;
        match self {
            Self::Open => Ok(Decision::Open {
                root: marked(),
                complete: layout.baseline_built(),
            }),
            Self::Reinitialize => Ok(Decision::Reinitialize(root(HERE))),
            Self::First => Ok(Decision::First),
            Self::Replace => Ok(Decision::Replace),
            Self::RebindBecauseSeveral => Ok(Decision::Rebind {
                reason: Reason::SeveralRoots,
                root: root(HERE),
            }),
            Self::RebindMarked => Ok(Decision::Rebind {
                reason: Reason::MarkedDirectory,
                root: marked(),
            }),
            Self::ForeignMarker => Err(RootError::ForeignMarker),
            Self::MissingMarker => Err(RootError::MissingMarker),
            Self::RootChanged => Err(RootError::RootChanged),
            Self::ReinitializeDenied => Err(RootError::ReinitializeDenied),
        }
    }
}

/// One group of combinations that share the decision `expect`.
struct Row {
    layouts: &'static [Layout],
    found: &'static [Found],
    expect: Expect,
}

fn decides_every_combination_once(ask: Ask, rows: &[Row]) {
    let mut seen = Vec::new();
    for row in rows {
        let before = seen.len();
        for &layout in row.layouts {
            for &found in row.found {
                if !found.exists_in(layout) {
                    continue;
                }
                assert!(
                    !seen.contains(&(layout, found)),
                    "{layout:?} with {found:?} is listed twice"
                );
                seen.push((layout, found));
                let marker = found.marker(layout);
                let disk = DiskFacts {
                    location: SELECTED,
                    node: node(100),
                    marker,
                };
                let facts = CatalogFacts {
                    id: catalog(THIS),
                    registered: layout.roots(),
                };
                assert_eq!(
                    decide(&disk, &facts, ask.activation()),
                    row.expect.resolve(layout, marker),
                    "{ask:?} with {layout:?} and {found:?}"
                );
            }
        }
        assert!(
            seen.len() > before,
            "{ask:?}: a row of {:?} with {:?} matches no combination",
            row.layouts,
            row.found
        );
    }
    for layout in LAYOUTS {
        for found in FOUND {
            if found.exists_in(layout) {
                assert!(
                    seen.contains(&(layout, found)),
                    "{ask:?} with {layout:?} and {found:?} has no row"
                );
            }
        }
    }
}

#[test]
fn opening_decides_every_layout_and_marker() {
    use Found::*;
    use Layout::*;
    decides_every_combination_once(
        Ask::Open,
        &[
            Row {
                layouts: &LAYOUTS,
                found: &[AnotherCatalogs],
                expect: Expect::ForeignMarker,
            },
            // The marker names the directory's own, only registered root: nothing changes.
            Row {
                layouts: &[SoleHere, SoleHereUnbuilt],
                found: &[OwnHere],
                expect: Expect::Open,
            },
            // A catalog of an earlier release registered several roots.
            Row {
                layouts: &[TwoOneHere],
                found: &[OwnHere],
                expect: Expect::RebindBecauseSeveral,
            },
            // A marker of a root other than the one registered at this directory.
            Row {
                layouts: &LAYOUTS,
                found: &[OwnElsewhere, OwnUnregistered],
                expect: Expect::RebindMarked,
            },
            // A registered directory that lost its marker needs an explicit reinitialization.
            Row {
                layouts: &[SoleHere, SoleHereUnbuilt, TwoOneHere],
                found: &[Nothing],
                expect: Expect::MissingMarker,
            },
            Row {
                layouts: &[Empty],
                found: &[Nothing],
                expect: Expect::First,
            },
            // Another directory in place of the registered ones: startup switches only if it
            // holds nothing but root files, which it proves after deciding.
            Row {
                layouts: &[SoleSamePath, SoleSameDirectory, SoleElsewhere, TwoElsewhere],
                found: &[Nothing],
                expect: Expect::Replace,
            },
        ],
    );
}

#[test]
fn reinitializing_this_catalog_decides_every_layout_and_marker() {
    use Found::*;
    use Layout::*;
    decides_every_combination_once(
        Ask::ReinitializeThisCatalog,
        &[
            Row {
                layouts: &LAYOUTS,
                found: &[AnotherCatalogs],
                expect: Expect::ForeignMarker,
            },
            Row {
                layouts: &[SoleHere, SoleHereUnbuilt],
                found: &[Nothing],
                expect: Expect::Reinitialize,
            },
            Row {
                layouts: &[TwoOneHere],
                found: &[Nothing],
                expect: Expect::RebindBecauseSeveral,
            },
            // Only the registered directory whose marker is missing may be reinitialized: one
            // that matches by path or by identity alone is a replaced directory.
            Row {
                layouts: &[SoleSamePath, SoleSameDirectory],
                found: &[Nothing],
                expect: Expect::RootChanged,
            },
            Row {
                layouts: &[Empty, SoleElsewhere, TwoElsewhere],
                found: &[Nothing],
                expect: Expect::ReinitializeDenied,
            },
            // A directory that still has its marker needs no reinitialization, and asking for
            // one is refused instead of being kept for the next time the marker goes missing.
            Row {
                layouts: &LAYOUTS,
                found: &[OwnHere, OwnElsewhere, OwnUnregistered],
                expect: Expect::ReinitializeDenied,
            },
        ],
    );
}

#[test]
fn reinitializing_another_catalog_is_refused_unless_it_is_taken_for_a_replaced_directory() {
    use Found::*;
    use Layout::*;
    decides_every_combination_once(
        Ask::ReinitializeAnotherCatalog,
        &[
            Row {
                layouts: &LAYOUTS,
                found: &[AnotherCatalogs],
                expect: Expect::ForeignMarker,
            },
            Row {
                layouts: &[SoleHere, SoleHereUnbuilt, TwoOneHere],
                found: &[Nothing],
                expect: Expect::ReinitializeDenied,
            },
            Row {
                layouts: &[SoleSamePath, SoleSameDirectory],
                found: &[Nothing],
                expect: Expect::RootChanged,
            },
            Row {
                layouts: &[Empty, SoleElsewhere, TwoElsewhere],
                found: &[Nothing],
                expect: Expect::ReinitializeDenied,
            },
            Row {
                layouts: &LAYOUTS,
                found: &[OwnHere, OwnElsewhere, OwnUnregistered],
                expect: Expect::ReinitializeDenied,
            },
        ],
    );
}
