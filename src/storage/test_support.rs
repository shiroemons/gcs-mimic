use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn downgrade_timestamp_precision(connection: &rusqlite::Connection) {
    connection.execute_batch(
        "UPDATE buckets SET time_created = time_created / 1000 - (time_created % 1000 < 0), updated = updated / 1000 - (updated % 1000 < 0);
         UPDATE objects SET time_created = time_created / 1000 - (time_created % 1000 < 0), updated = updated / 1000 - (updated % 1000 < 0), time_deleted = time_deleted / 1000 - (time_deleted % 1000 < 0);"
    ).unwrap();
}

pub fn floor_to_millis(time: SystemTime) -> SystemTime {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => UNIX_EPOCH + Duration::from_millis(duration.as_millis() as u64),
        Err(error) => {
            let millis = error.duration().as_nanos().div_ceil(1_000_000);
            UNIX_EPOCH - Duration::from_millis(millis as u64)
        }
    }
}
