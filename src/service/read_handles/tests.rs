use super::*;
use std::time::Duration;

#[test]
fn read_handle_table_evicts_the_oldest_entry_at_its_bound() {
    let handles = ReadHandles::default();
    let now = SystemTime::now();
    let token = |index: usize| {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&u64::try_from(index).unwrap().to_le_bytes());
        HandleToken(bytes)
    };
    for index in 0..=MAX_HANDLES {
        handles.insert(
            Handle {
                token: token(index),
                bucket: "bucket".into(),
                name: "object".into(),
                generation: 1,
                size: 0,
                finality: Finality::Finalized,
                project: None,
                expires: now + Duration::from_secs(600),
            },
            now,
        );
    }
    assert!(handles.find(&token(0).0, now).is_none());
    assert!(handles.find(&token(MAX_HANDLES).0, now).is_some());
    assert_eq!(handles.0.lock().unwrap().len(), MAX_HANDLES);
}

#[test]
fn handle_tokens_reject_every_changed_byte_and_wrong_length() {
    let token = HandleToken([0xa5; 32]);
    assert!(token.matches(&token.0));
    for index in 0..32 {
        let mut changed = token.0;
        changed[index] ^= 1;
        assert!(!token.matches(&changed));
    }
    assert!(!token.matches(&token.0[..31]));
    assert!(!token.matches(&[0xa5; 33]));
}
