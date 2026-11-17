use crate::auth::{SecretHash, hash_secret, valid_username};
use ome_protocol::{Application, MemberKind};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub kind: MemberKind,
    pub secret: SecretHash,
    pub received_at: u64,
    pub note: String,
}

#[derive(Debug, Default)]
pub struct Applications {
    pending: BTreeMap<String, Pending>,
}

const LIMIT: usize = 256;

impl Applications {
    pub fn receive(
        &mut self,
        kind: MemberKind,
        name: &str,
        secret: &str,
        note: &str,
        now: u64,
    ) -> Result<String, String> {
        if !valid_username(name) || secret.is_empty() {
            return Err("applicant name must be 1-32 chars of a-z, 0-9, _ or - and secret non-empty".into());
        }
        if self.pending.len() >= LIMIT && !self.pending.contains_key(name) {
            return Err("too many pending applications; try again later".into());
        }
        let again = self
            .pending
            .insert(
                name.to_string(),
                Pending {
                    kind,
                    secret: hash_secret(name, secret),
                    received_at: now,
                    note: note.chars().take(200).collect(),
                },
            )
            .is_some();
        Ok(if again {
            format!("application from {name} renewed; awaiting review")
        } else {
            format!("application from {name} received; awaiting review")
        })
    }

    pub fn take(&mut self, name: &str) -> Option<Pending> {
        self.pending.remove(name)
    }

    pub fn restore(&mut self, name: &str, pending: Pending) {
        self.pending.entry(name.to_string()).or_insert(pending);
    }

    pub fn withdraw(&mut self, name: &str) {
        self.pending.remove(name);
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn list(&self) -> Vec<Application> {
        self.pending
            .iter()
            .map(|(name, p)| Application {
                name: name.clone(),
                kind: p.kind,
                received_at: p.received_at,
                note: p.note.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::verify;

    #[test]
    fn applications_wait_for_review_and_keep_only_a_secret_hash() {
        let mut book = Applications::default();
        assert!(book.receive(MemberKind::Broker, "Bad Name", "pw", "", 1).is_err());
        assert!(book.receive(MemberKind::Broker, "retail-co", "", "", 1).is_err());
        assert!(book.receive(MemberKind::Broker, "retail-co", "pw", "retail broker", 1).unwrap().contains("received"));
        assert!(book.receive(MemberKind::Broker, "retail-co", "pw2", "retail broker", 5).unwrap().contains("renewed"));
        assert_eq!(book.list()[0].received_at, 5);
        let pending = book.take("retail-co").unwrap();
        assert!(verify(&pending.secret, "retail-co", "pw2"));
        assert!(book.is_empty() && book.take("retail-co").is_none());
    }
}
