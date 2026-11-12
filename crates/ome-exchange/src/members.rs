use ome_net::auth::SecretHash;
use ome_protocol::{MemberId, MemberKind};
use std::collections::{BTreeMap, HashMap};

pub struct Member {
    pub id: MemberId,
    pub name: String,
    pub kind: MemberKind,
    pub secret: SecretHash,
}

#[derive(Default)]
pub struct Members {
    by_id: BTreeMap<MemberId, Member>,
    by_name: HashMap<String, MemberId>,
    next_id: MemberId,
}

impl Members {
    pub fn admit(&mut self, name: &str, kind: MemberKind, secret: SecretHash) -> Option<MemberId> {
        if self.by_name.contains_key(name) {
            return None;
        }
        self.next_id += 1;
        let id = self.next_id;
        self.by_name.insert(name.to_string(), id);
        self.by_id.insert(
            id,
            Member {
                id,
                name: name.to_string(),
                kind,
                secret,
            },
        );
        Some(id)
    }

    pub fn get(&self, id: MemberId) -> Option<&Member> {
        self.by_id.get(&id)
    }

    pub fn find(&self, name: &str) -> Option<&Member> {
        self.by_name.get(name).and_then(|id| self.by_id.get(id))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Member> {
        self.by_id.values()
    }
}
