use crate::catalog::ObjectRecord;

#[derive(Default, Debug, Eq, PartialEq)]
pub struct SyncSummary {
    pub received: usize,
    pub sent: usize,
    pub deleted: usize,
}

pub(super) enum Winner<'a> {
    Local(&'a ObjectRecord),
    Remote(&'a ObjectRecord),
    Equal,
}

pub(super) fn winner<'a>(
    local: &'a Option<ObjectRecord>,
    remote: &'a Option<ObjectRecord>,
) -> Winner<'a> {
    match (local, remote) {
        (Some(local), Some(remote)) if local.version > remote.version => Winner::Local(local),
        (Some(local), Some(remote)) if remote.version > local.version => Winner::Remote(remote),
        (Some(_), Some(_)) => Winner::Equal,
        (Some(local), None) => Winner::Local(local),
        (None, Some(remote)) => Winner::Remote(remote),
        (None, None) => Winner::Equal,
    }
}
