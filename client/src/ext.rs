use anyhow::Result;
use rusty_ulid::Ulid;
use std::{str::FromStr, time::Duration};

pub trait ClientJobExt {
    fn duration(&self, from: &str, until: &str) -> Option<Duration>;
}

impl ClientJobExt for crate::types::Job {
    fn duration(&self, from: &str, until: &str) -> Option<Duration> {
        let from = if let Some(from) = self.times.get(from) {
            from
        } else {
            return None;
        };
        let until = if let Some(until) = self.times.get(until) {
            until
        } else {
            return None;
        };

        if let Ok(dur) = until.signed_duration_since(*from).to_std() {
            if dur.is_zero() {
                None
            } else {
                Some(dur)
            }
        } else {
            None
        }
    }
}

pub trait ClientIdExt {
    fn id(&self) -> Result<Ulid>;
}

impl ClientIdExt for crate::types::Worker {
    fn id(&self) -> Result<Ulid> {
        to_ulid(&self.id)
    }
}

impl ClientIdExt for crate::types::Job {
    fn id(&self) -> Result<Ulid> {
        to_ulid(&self.id)
    }
}

fn to_ulid(id: &str) -> Result<Ulid> {
    Ok(Ulid::from_str(id)?)
}
