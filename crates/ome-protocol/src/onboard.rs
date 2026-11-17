use crate::codec::{DecodeError, Decoder, Encoder};
use crate::types::{MemberKind, member_kind_from};
use ome_core::types::Timestamp;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Application {
    pub name: String,
    pub kind: MemberKind,
    pub received_at: Timestamp,
    pub note: String,
}

pub fn put_applications(e: &mut Encoder, list: &[Application]) {
    e.end_block();
    let mut g = e.begin_group(list.len());
    for a in list {
        e.begin_entry(&mut g);
        e.u8(a.kind as u8).u64(a.received_at);
        e.end_entry_block(&mut g);
        e.var(&a.name).var(&a.note);
    }
}

pub fn get_applications(d: &mut Decoder) -> Result<Vec<Application>, DecodeError> {
    d.end_block()?;
    let g = d.group()?;
    let mut list = Vec::with_capacity(g.count);
    for _ in 0..g.count {
        let start = d.entry_start();
        let kind = member_kind_from(d.u8()?)?;
        let received_at = d.u64()?;
        d.end_entry_block(start, &g)?;
        list.push(Application {
            name: d.var()?,
            kind,
            received_at,
            note: d.var()?,
        });
    }
    Ok(list)
}
