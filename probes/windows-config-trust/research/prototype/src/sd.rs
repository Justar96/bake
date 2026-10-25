//! Pure, bounds-checked evaluation of a self-relative Windows security
//! descriptor (MS-DTYP 2.4.6) for "may anyone but a trusted principal
//! change this object, or what its name resolves to?".
//!
//! Nothing here calls Windows: the FFI layer only fetches the descriptor
//! bytes with `GetKernelObjectSecurity(handle, OWNER | DACL | LABEL)`. That
//! keeps every rule unit-testable on every platform and makes malformed or
//! hostile bytes an error, never a panic.
//!
//! The evaluation over-approximates who can change the object:
//! - every allow ACE that is not inherit-only counts, including callback
//!   (conditional) and object ACEs, as if its condition held;
//! - deny ACEs, central access policies, and their order are ignored,
//!   since they can only narrow access;
//! - generic bits are mapped with the file generic mapping, and
//!   `MAXIMUM_ALLOWED` counts as every right;
//! - the owner is implicitly granted `READ_CONTROL | WRITE_DAC`
//!   (MS-DTYP 2.5.3.2), so a foreign owner is a foreign writer.
//!
//! Unknown ACE types in the DACL, a NULL or absent DACL, and malformed
//! structures refuse.

#![allow(dead_code)]

use std::fmt;

/// A SID, kept as its canonical bytes (revision, count, authority,
/// sub-authorities), which is what `EqualSid` compares.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Sid(Vec<u8>);

impl Sid {
    pub fn new(authority: u64, sub: &[u32]) -> Self {
        assert!(sub.len() <= 15 && authority < (1 << 48));
        let mut bytes = vec![1u8, sub.len() as u8];
        bytes.extend_from_slice(&authority.to_be_bytes()[2..]);
        for value in sub {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        Sid(bytes)
    }

    /// Parses a SID at the start of `bytes`; returns it and its length.
    pub fn parse(bytes: &[u8]) -> Result<(Sid, usize), Malformed> {
        let header = bytes.get(..8).ok_or(Malformed("SID header"))?;
        if header[0] != 1 {
            return Err(Malformed("SID revision"));
        }
        let count = usize::from(header[1]);
        if count > 15 {
            return Err(Malformed("SID sub-authority count"));
        }
        let length = 8 + 4 * count;
        let all = bytes.get(..length).ok_or(Malformed("SID length"))?;
        Ok((Sid(all.to_vec()), length))
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    fn authority(&self) -> u64 {
        self.0[2..8]
            .iter()
            .fold(0u64, |value, byte| (value << 8) | u64::from(*byte))
    }

    pub fn sub_authorities(&self) -> impl Iterator<Item = u32> + '_ {
        self.0[8..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| u32::from_le_bytes(*chunk))
    }

    /// The last sub-authority (an integrity level's RID for S-1-16-x).
    fn last(&self) -> Option<u32> {
        self.sub_authorities().last()
    }
}

impl fmt::Display for Sid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let authority = self.authority();
        if authority < (1 << 32) {
            write!(f, "S-1-{authority}")?;
        } else {
            write!(f, "S-1-0x{authority:012X}")?;
        }
        for value in self.sub_authorities() {
            write!(f, "-{value}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Sid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

pub mod well_known {
    use super::Sid;
    pub fn local_system() -> Sid {
        Sid::new(5, &[18])
    }
    pub fn administrators() -> Sid {
        Sid::new(5, &[32, 544])
    }
    /// `NT SERVICE\TrustedInstaller`.
    pub fn trusted_installer() -> Sid {
        Sid::new(
            5,
            &[80, 956_008_885, 3_418_522_649, 1_831_038_044, 1_853_292_631, 2_271_478_464],
        )
    }
    pub fn owner_rights() -> Sid {
        Sid::new(3, &[4])
    }
    pub fn creator_owner() -> Sid {
        Sid::new(3, &[0])
    }
    pub fn everyone() -> Sid {
        Sid::new(1, &[0])
    }
    pub fn authenticated_users() -> Sid {
        Sid::new(5, &[11])
    }
    pub fn users() -> Sid {
        Sid::new(5, &[32, 545])
    }
    pub fn all_application_packages() -> Sid {
        Sid::new(15, &[2, 1])
    }
}

/// The principals that may hold change rights: the token user, LocalSystem,
/// BUILTIN\Administrators, and TrustedInstaller (the Unix analogue of "the
/// user or root").
#[derive(Clone, Debug)]
pub struct Principals {
    trusted: Vec<Sid>,
}

impl Principals {
    pub fn new(user: Sid) -> Self {
        Self {
            trusted: vec![
                user,
                well_known::local_system(),
                well_known::administrators(),
                well_known::trusted_installer(),
            ],
        }
    }

    pub fn is_trusted(&self, sid: &Sid) -> bool {
        self.trusted.contains(sid)
    }
}

/// What the object is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
}

/// Where the object sits on the path from the volume root to the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// The Bake home and everything below it: no foreign change at all.
    Managed,
    /// A directory above the home, held open with its child: others may
    /// add entries, which they own and which the walk never opens, but may
    /// not remove, rename, or replace an existing one, or change the
    /// directory's attributes or security.
    Ancestor,
    /// The volume root: as `Ancestor`, and `DELETE` on it is ignored since
    /// "a volume's root directory cannot be renamed" (FILE_RENAME_INFORMATION)
    /// and it is never empty while the walk holds a child.
    VolumeRoot,
}

pub mod rights {
    pub const FILE_READ_DATA: u32 = 0x1;
    pub const FILE_WRITE_DATA: u32 = 0x2; // ADD_FILE on a directory
    pub const FILE_APPEND_DATA: u32 = 0x4; // ADD_SUBDIRECTORY on a directory
    pub const FILE_READ_EA: u32 = 0x8;
    pub const FILE_WRITE_EA: u32 = 0x10;
    pub const FILE_EXECUTE: u32 = 0x20; // TRAVERSE on a directory
    pub const FILE_DELETE_CHILD: u32 = 0x40;
    pub const FILE_READ_ATTRIBUTES: u32 = 0x80;
    pub const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
    pub const DELETE: u32 = 0x1_0000;
    pub const READ_CONTROL: u32 = 0x2_0000;
    pub const WRITE_DAC: u32 = 0x4_0000;
    pub const WRITE_OWNER: u32 = 0x8_0000;
    pub const SYNCHRONIZE: u32 = 0x10_0000;
    pub const ACCESS_SYSTEM_SECURITY: u32 = 0x100_0000;
    pub const MAXIMUM_ALLOWED: u32 = 0x200_0000;
    pub const GENERIC_ALL: u32 = 0x1000_0000;
    pub const GENERIC_EXECUTE: u32 = 0x2000_0000;
    pub const GENERIC_WRITE: u32 = 0x4000_0000;
    pub const GENERIC_READ: u32 = 0x8000_0000;
    pub const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
    pub const FILE_GENERIC_READ: u32 = 0x0012_0089;
    pub const FILE_GENERIC_WRITE: u32 = 0x0012_0116;
    pub const FILE_GENERIC_EXECUTE: u32 = 0x0012_00A0;

    /// Every right that changes an object's bytes, attributes, name, or
    /// security, or what a directory's names resolve to.
    pub const CHANGES: u32 = FILE_WRITE_DATA
        | FILE_APPEND_DATA
        | FILE_WRITE_EA
        | FILE_DELETE_CHILD
        | FILE_WRITE_ATTRIBUTES
        | DELETE
        | WRITE_DAC
        | WRITE_OWNER
        | ACCESS_SYSTEM_SECURITY;

    /// Mapped with the file generic mapping (File Security and Access
    /// Rights); `MAXIMUM_ALLOWED` counts as every right.
    pub fn map_generic(mask: u32) -> u32 {
        let mut mapped = mask & !(GENERIC_ALL | GENERIC_EXECUTE | GENERIC_WRITE | GENERIC_READ);
        if mask & (GENERIC_ALL | MAXIMUM_ALLOWED) != 0 {
            mapped |= FILE_ALL_ACCESS | ACCESS_SYSTEM_SECURITY;
        }
        if mask & GENERIC_WRITE != 0 {
            mapped |= FILE_GENERIC_WRITE;
        }
        if mask & GENERIC_READ != 0 {
            mapped |= FILE_GENERIC_READ;
        }
        if mask & GENERIC_EXECUTE != 0 {
            mapped |= FILE_GENERIC_EXECUTE;
        }
        mapped & !MAXIMUM_ALLOWED
    }
}

/// The change rights a foreign principal may hold at `place`.
pub fn tolerated(kind: Kind, place: Place) -> u32 {
    use rights::*;
    match (kind, place) {
        (_, Place::Managed) | (Kind::File, _) => 0,
        (Kind::Directory, Place::Ancestor) => FILE_WRITE_DATA | FILE_APPEND_DATA,
        (Kind::Directory, Place::VolumeRoot) => FILE_WRITE_DATA | FILE_APPEND_DATA | DELETE,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Malformed(pub &'static str);

/// Why an object is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Malformed(&'static str),
    NoOwner,
    ForeignOwner(Sid),
    /// `SE_DACL_PRESENT` clear, or set with no ACL: everyone has full access.
    NullDacl,
    UnknownAce { index: usize, ace_type: u8 },
    ForeignChange { index: usize, sid: Sid, rights: u32 },
    /// A mandatory label below Medium: lower-integrity processes of the
    /// same user may write it when the DACL allows.
    LowIntegrity { rid: u32 },
}

impl From<Malformed> for Refusal {
    fn from(malformed: Malformed) -> Self {
        Refusal::Malformed(malformed.0)
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Malformed(what) => write!(f, "its security descriptor cannot be read ({what})"),
            Refusal::NoOwner => f.write_str("it has no owner"),
            Refusal::ForeignOwner(sid) => write!(f, "it is owned by {sid}, not by you, SYSTEM, Administrators, or TrustedInstaller"),
            Refusal::NullDacl => f.write_str("it has no access control list, so everyone may change it"),
            Refusal::UnknownAce { index, ace_type } => write!(f, "its ACE {index} has a type ({ace_type:#04x}) bake-rs does not evaluate"),
            Refusal::ForeignChange { index, sid, rights } => write!(f, "its ACE {index} lets {sid} change it (rights {rights:#010x})"),
            Refusal::LowIntegrity { rid } => write!(f, "its integrity label ({rid:#x}) is below Medium"),
        }
    }
}

/// One ACE as read, for diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AceView {
    pub ace_type: u8,
    pub flags: u8,
    pub mask: u32,
    pub sid: Option<Sid>,
}

/// The parsed descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub control: u16,
    pub owner: Option<Sid>,
    /// `None` is a NULL or absent DACL.
    pub dacl: Option<Vec<AceView>>,
    pub label_rid: Option<u32>,
}

const SE_DACL_PRESENT: u16 = 0x0004;
const SE_SACL_PRESENT: u16 = 0x0010;
const SE_SELF_RELATIVE: u16 = 0x8000;
const INHERIT_ONLY_ACE: u8 = 0x08;
const MAX_ACES: usize = 4096;

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, Malformed> {
    let b = bytes.get(at..at + 2).ok_or(Malformed("truncated"))?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, Malformed> {
    let b = bytes.get(at..at + 4).ok_or(Malformed("truncated"))?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// One ACE: its type, flags, and whole bytes.
type RawAce<'a> = (u8, u8, &'a [u8]);

/// The ACL at `offset`: each ACE's header fields and body.
fn acl(bytes: &[u8], offset: usize) -> Result<Vec<RawAce<'_>>, Malformed> {
    let header = bytes.get(offset..offset + 8).ok_or(Malformed("ACL header"))?;
    if header[0] != 2 && header[0] != 4 {
        return Err(Malformed("ACL revision"));
    }
    let size = usize::from(u16::from_le_bytes([header[2], header[3]]));
    let count = usize::from(u16::from_le_bytes([header[4], header[5]]));
    if size < 8 || count > MAX_ACES {
        return Err(Malformed("ACL size"));
    }
    let body = bytes.get(offset..offset + size).ok_or(Malformed("ACL size"))?;
    let mut at = 8;
    let mut aces = Vec::with_capacity(count);
    for _ in 0..count {
        let head = body.get(at..at + 4).ok_or(Malformed("ACE header"))?;
        let ace_size = usize::from(u16::from_le_bytes([head[2], head[3]]));
        if ace_size < 8 || ace_size % 4 != 0 {
            return Err(Malformed("ACE size"));
        }
        let ace = body.get(at..at + ace_size).ok_or(Malformed("ACE beyond ACL"))?;
        aces.push((head[0], head[1], ace));
        at += ace_size;
    }
    Ok(aces)
}

/// The mask and SID of an ACE that names a SID, by type.
fn mask_and_sid(ace_type: u8, ace: &[u8]) -> Result<Option<(u32, Sid)>, Malformed> {
    let mask = u32_at(ace, 4)?;
    let sid_at = match ace_type {
        // allowed, denied, allowed-callback, denied-callback
        0x0 | 0x1 | 0x9 | 0xA => 8,
        // object variants: Flags, then optional ObjectType and
        // InheritedObjectType GUIDs
        0x5 | 0x6 | 0xB | 0xC => {
            let flags = u32_at(ace, 8)?;
            12 + if flags & 1 != 0 { 16 } else { 0 } + if flags & 2 != 0 { 16 } else { 0 }
        }
        _ => return Ok(None),
    };
    let (sid, _) = Sid::parse(ace.get(sid_at..).ok_or(Malformed("ACE SID"))?)?;
    Ok(Some((mask, sid)))
}

pub fn parse(bytes: &[u8]) -> Result<Descriptor, Malformed> {
    let header = bytes.get(..20).ok_or(Malformed("descriptor header"))?;
    if header[0] != 1 {
        return Err(Malformed("descriptor revision"));
    }
    let control = u16_at(bytes, 2)?;
    if control & SE_SELF_RELATIVE == 0 {
        return Err(Malformed("not self-relative"));
    }
    let owner_at = u32_at(bytes, 4)? as usize;
    let sacl_at = u32_at(bytes, 12)? as usize;
    let dacl_at = u32_at(bytes, 16)? as usize;
    let owner = if owner_at == 0 {
        None
    } else {
        Some(Sid::parse(bytes.get(owner_at..).ok_or(Malformed("owner offset"))?)?.0)
    };
    let dacl = if control & SE_DACL_PRESENT == 0 || dacl_at == 0 {
        None
    } else {
        let mut views = Vec::new();
        for (ace_type, flags, ace) in acl(bytes, dacl_at)? {
            let sid = mask_and_sid(ace_type, ace)?;
            views.push(AceView {
                ace_type,
                flags,
                mask: sid.as_ref().map_or(u32_at(ace, 4)?, |(mask, _)| *mask),
                sid: sid.map(|(_, sid)| sid),
            });
        }
        Some(views)
    };
    let mut label_rid = None;
    if control & SE_SACL_PRESENT != 0 && sacl_at != 0 {
        for (ace_type, flags, ace) in acl(bytes, sacl_at)? {
            // SYSTEM_MANDATORY_LABEL_ACE: header, policy mask, S-1-16-RID.
            if ace_type == 0x11 && flags & INHERIT_ONLY_ACE == 0 {
                let (sid, _) = Sid::parse(ace.get(8..).ok_or(Malformed("label SID"))?)?;
                if sid.authority() != 16 {
                    return Err(Malformed("label authority"));
                }
                label_rid = Some(sid.last().ok_or(Malformed("label RID"))?);
            }
        }
    }
    Ok(Descriptor {
        control,
        owner,
        dacl,
        label_rid,
    })
}

/// What evaluation found on an object that passed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Passed {
    /// Foreign add-entry rights tolerated on an ancestor or volume root.
    pub tolerated_foreign: u32,
    pub aces: usize,
}

/// Evaluates the descriptor bytes of an object of `kind` at `place`.
pub fn evaluate(
    bytes: &[u8],
    kind: Kind,
    place: Place,
    principals: &Principals,
) -> Result<Passed, Refusal> {
    let descriptor = parse(bytes)?;
    let owner = descriptor.owner.as_ref().ok_or(Refusal::NoOwner)?;
    if !principals.is_trusted(owner) {
        return Err(Refusal::ForeignOwner(owner.clone()));
    }
    if let Some(rid) = descriptor.label_rid
        && rid < 0x2000
    {
        return Err(Refusal::LowIntegrity { rid });
    }
    let aces = descriptor.dacl.as_ref().ok_or(Refusal::NullDacl)?;
    let tolerated = tolerated(kind, place);
    let mut passed = Passed {
        aces: aces.len(),
        ..Passed::default()
    };
    let owner_rights = well_known::owner_rights();
    for (index, ace) in aces.iter().enumerate() {
        if ace.flags & INHERIT_ONLY_ACE != 0 {
            continue;
        }
        match ace.ace_type {
            // Allow: plain, object, callback, callback-object.
            0x0 | 0x5 | 0x9 | 0xB => {}
            // Deny variants only narrow access.
            0x1 | 0x6 | 0xA | 0xC => continue,
            ace_type => return Err(Refusal::UnknownAce { index, ace_type }),
        }
        let Some(sid) = ace.sid.as_ref() else {
            return Err(Refusal::UnknownAce {
                index,
                ace_type: ace.ace_type,
            });
        };
        // OWNER RIGHTS stands for the owner, which is trusted here.
        if principals.is_trusted(sid) || *sid == owner_rights {
            continue;
        }
        let changes = rights::map_generic(ace.mask) & rights::CHANGES;
        let refused = changes & !tolerated;
        if refused != 0 {
            return Err(Refusal::ForeignChange {
                index,
                sid: sid.clone(),
                rights: refused,
            });
        }
        passed.tolerated_foreign |= changes;
    }
    Ok(passed)
}

/// Builders for synthetic descriptors, used by the tests and by the probe's
/// self-check.
pub mod build {
    use super::Sid;

    pub struct Ace {
        pub ace_type: u8,
        pub flags: u8,
        pub mask: u32,
        pub sid: Sid,
    }

    pub fn allow(flags: u8, mask: u32, sid: Sid) -> Ace {
        Ace {
            ace_type: 0,
            flags,
            mask,
            sid,
        }
    }

    pub fn deny(flags: u8, mask: u32, sid: Sid) -> Ace {
        Ace {
            ace_type: 1,
            flags,
            mask,
            sid,
        }
    }

    pub fn acl(aces: &[Ace]) -> Vec<u8> {
        let mut body = Vec::new();
        for ace in aces {
            let mut entry = vec![ace.ace_type, ace.flags, 0, 0];
            entry.extend_from_slice(&ace.mask.to_le_bytes());
            if matches!(ace.ace_type, 0x5 | 0x6 | 0xB | 0xC) {
                entry.extend_from_slice(&0u32.to_le_bytes());
            }
            entry.extend_from_slice(ace.sid.bytes());
            while entry.len() % 4 != 0 {
                entry.push(0);
            }
            let size = entry.len() as u16;
            entry[2..4].copy_from_slice(&size.to_le_bytes());
            body.extend(entry);
        }
        let mut acl = vec![2u8, 0];
        acl.extend_from_slice(&((body.len() + 8) as u16).to_le_bytes());
        acl.extend_from_slice(&(aces.len() as u16).to_le_bytes());
        acl.extend_from_slice(&[0, 0]);
        acl.extend(body);
        acl
    }

    /// A self-relative descriptor. `dacl` `None` leaves `SE_DACL_PRESENT`
    /// clear; `Some(None)` sets it with no ACL (a NULL DACL).
    pub fn descriptor(owner: Option<&Sid>, dacl: Option<Option<&[Ace]>>, label: Option<u32>) -> Vec<u8> {
        let mut control: u16 = 0x8000;
        let mut tail = Vec::new();
        let mut owner_at = 0u32;
        let mut dacl_at = 0u32;
        let mut sacl_at = 0u32;
        if let Some(owner) = owner {
            owner_at = (20 + tail.len()) as u32;
            tail.extend_from_slice(owner.bytes());
        }
        if let Some(dacl) = dacl {
            control |= 0x0004;
            if let Some(aces) = dacl {
                dacl_at = (20 + tail.len()) as u32;
                tail.extend(acl(aces));
            }
        }
        if let Some(rid) = label {
            control |= 0x0010;
            sacl_at = (20 + tail.len()) as u32;
            tail.extend(acl(&[Ace {
                ace_type: 0x11,
                flags: 0,
                mask: 1,
                sid: Sid::new(16, &[rid]),
            }]));
        }
        let mut sd = vec![1u8, 0];
        sd.extend_from_slice(&control.to_le_bytes());
        sd.extend_from_slice(&owner_at.to_le_bytes());
        sd.extend_from_slice(&0u32.to_le_bytes());
        sd.extend_from_slice(&sacl_at.to_le_bytes());
        sd.extend_from_slice(&dacl_at.to_le_bytes());
        sd.extend(tail);
        sd
    }
}

#[cfg(test)]
mod tests {
    use super::build::{Ace, allow, deny, descriptor};
    use super::rights::*;
    use super::well_known::*;
    use super::*;

    const OI: u8 = 0x1;
    const CI: u8 = 0x2;
    const IO: u8 = 0x8;
    const INH: u8 = 0x10;

    fn user() -> Sid {
        Sid::new(5, &[21, 1, 2, 3, 1001])
    }

    fn other_user() -> Sid {
        Sid::new(5, &[21, 1, 2, 3, 1002])
    }

    fn principals() -> Principals {
        Principals::new(user())
    }

    fn check(owner: &Sid, aces: &[Ace], kind: Kind, place: Place) -> Result<Passed, Refusal> {
        evaluate(&descriptor(Some(owner), Some(Some(aces)), None), kind, place, &principals())
    }

    fn profile() -> Vec<Ace> {
        vec![
            allow(OI | CI | INH, FILE_ALL_ACCESS, local_system()),
            allow(OI | CI | INH, FILE_ALL_ACCESS, administrators()),
            allow(OI | CI | INH, FILE_ALL_ACCESS, user()),
        ]
    }

    /// The commonly reported stock `C:\` DACL (pending native evidence).
    fn system_drive_root() -> Vec<Ace> {
        vec![
            allow(OI | CI, FILE_ALL_ACCESS, administrators()),
            allow(OI | CI, FILE_ALL_ACCESS, local_system()),
            allow(OI | CI, FILE_GENERIC_READ | FILE_GENERIC_EXECUTE, users()),
            allow(OI | CI | IO, 0x0013_01BF, authenticated_users()), // Modify, children only
            allow(0, FILE_APPEND_DATA, authenticated_users()),       // create folders here
            allow(OI | CI | IO, GENERIC_ALL, creator_owner()),
        ]
    }

    #[test]
    fn a_stock_profile_passes_everywhere() {
        for place in [Place::Managed, Place::Ancestor, Place::VolumeRoot] {
            for kind in [Kind::File, Kind::Directory] {
                assert!(check(&user(), &profile(), kind, place).is_ok());
            }
        }
    }

    #[test]
    fn the_system_drive_root_passes_only_as_root_or_ancestor() {
        let owner = trusted_installer();
        let root = check(&owner, &system_drive_root(), Kind::Directory, Place::VolumeRoot).unwrap();
        assert_eq!(root.tolerated_foreign, FILE_APPEND_DATA);
        assert!(check(&owner, &system_drive_root(), Kind::Directory, Place::Ancestor).is_ok());
        assert_eq!(
            check(&owner, &system_drive_root(), Kind::Directory, Place::Managed),
            Err(Refusal::ForeignChange {
                index: 4,
                sid: authenticated_users(),
                rights: FILE_APPEND_DATA
            })
        );
    }

    #[test]
    fn an_inherited_modify_grant_refuses_a_home_created_under_a_drive_root() {
        // `C:\bake`, created by the user, inherits Authenticated Users Modify.
        let mut aces = profile();
        aces.push(allow(OI | CI | INH, 0x0013_01BF, authenticated_users()));
        let refused = check(&user(), &aces, Kind::Directory, Place::Managed).unwrap_err();
        assert!(matches!(refused, Refusal::ForeignChange { .. }));
        // Modify includes DELETE and attribute writes, which no ancestor tolerates.
        assert!(check(&user(), &aces, Kind::Directory, Place::Ancestor).is_err());
    }

    #[test]
    fn owners_and_dacl_presence() {
        let p = principals();
        assert_eq!(
            evaluate(&descriptor(None, Some(Some(&profile())), None), Kind::File, Place::Managed, &p),
            Err(Refusal::NoOwner)
        );
        assert_eq!(
            check(&other_user(), &profile(), Kind::File, Place::Managed),
            Err(Refusal::ForeignOwner(other_user()))
        );
        for owner in [local_system(), administrators(), trusted_installer()] {
            assert!(check(&owner, &profile(), Kind::File, Place::Managed).is_ok());
        }
        assert_eq!(
            evaluate(&descriptor(Some(&user()), None, None), Kind::File, Place::Managed, &p),
            Err(Refusal::NullDacl)
        );
        assert_eq!(
            evaluate(&descriptor(Some(&user()), Some(None), None), Kind::File, Place::Managed, &p),
            Err(Refusal::NullDacl)
        );
        // An empty DACL leaves only the owner's implicit rights.
        assert!(evaluate(&descriptor(Some(&user()), Some(Some(&[])), None), Kind::File, Place::Managed, &p).is_ok());
    }

    #[test]
    fn reads_are_never_refused_and_writes_by_others_always_are_on_files() {
        let mut aces = profile();
        aces.push(allow(0, GENERIC_READ | GENERIC_EXECUTE, everyone()));
        aces.push(allow(0, FILE_GENERIC_READ, other_user()));
        assert!(check(&user(), &aces, Kind::File, Place::Managed).is_ok());
        for mask in [
            FILE_WRITE_DATA,
            FILE_APPEND_DATA,
            FILE_WRITE_EA,
            FILE_WRITE_ATTRIBUTES,
            DELETE,
            WRITE_DAC,
            WRITE_OWNER,
            GENERIC_WRITE,
            GENERIC_ALL,
            MAXIMUM_ALLOWED,
            ACCESS_SYSTEM_SECURITY,
        ] {
            let mut aces = profile();
            aces.push(allow(0, mask, other_user()));
            assert!(
                matches!(check(&user(), &aces, Kind::File, Place::Managed), Err(Refusal::ForeignChange { .. })),
                "{mask:#x}"
            );
        }
    }

    #[test]
    fn ancestors_tolerate_adding_but_not_replacing() {
        let add = FILE_WRITE_DATA | FILE_APPEND_DATA | FILE_GENERIC_READ;
        let mut aces = profile();
        aces.push(allow(0, add, everyone()));
        assert!(check(&user(), &aces, Kind::Directory, Place::Ancestor).is_ok());
        assert!(check(&user(), &aces, Kind::Directory, Place::Managed).is_err());
        for mask in [FILE_DELETE_CHILD, DELETE, WRITE_DAC, WRITE_OWNER, FILE_WRITE_ATTRIBUTES, FILE_WRITE_EA, GENERIC_WRITE] {
            let mut aces = profile();
            aces.push(allow(0, add | mask, everyone()));
            assert!(check(&user(), &aces, Kind::Directory, Place::Ancestor).is_err(), "{mask:#x}");
        }
        // DELETE on the volume root is ignored: the root cannot be renamed.
        let mut aces = profile();
        aces.push(allow(0, add | DELETE, authenticated_users()));
        assert!(check(&user(), &aces, Kind::Directory, Place::VolumeRoot).is_ok());
        assert!(check(&user(), &aces, Kind::Directory, Place::Ancestor).is_err());
    }

    #[test]
    fn inherit_only_and_deny_entries() {
        let mut aces = profile();
        aces.push(allow(OI | CI | IO, FILE_ALL_ACCESS, everyone()));
        assert!(check(&user(), &aces, Kind::Directory, Place::Managed).is_ok());
        // A deny before an allow does not rescue it: order is not evaluated.
        let mut aces = vec![deny(0, FILE_WRITE_DATA, everyone())];
        aces.extend(profile());
        aces.push(allow(0, FILE_WRITE_DATA, everyone()));
        assert!(check(&user(), &aces, Kind::File, Place::Managed).is_err());
        let mut aces = profile();
        aces.push(deny(0, FILE_ALL_ACCESS, everyone()));
        assert!(check(&user(), &aces, Kind::File, Place::Managed).is_ok());
    }

    #[test]
    fn callback_object_and_unknown_aces() {
        for ace_type in [0x5u8, 0x9, 0xB] {
            let mut aces = profile();
            aces.push(Ace { ace_type, flags: 0, mask: FILE_WRITE_DATA, sid: other_user() });
            assert!(matches!(check(&user(), &aces, Kind::File, Place::Managed), Err(Refusal::ForeignChange { .. })), "{ace_type:#x}");
            let mut aces = profile();
            aces.push(Ace { ace_type, flags: 0, mask: FILE_GENERIC_READ, sid: other_user() });
            assert!(check(&user(), &aces, Kind::File, Place::Managed).is_ok());
        }
        for ace_type in [0x2u8, 0x3, 0x4, 0x7, 0x8, 0xD, 0x11, 0x12, 0x13, 0x14, 0xFF] {
            let mut aces = profile();
            aces.push(Ace { ace_type, flags: 0, mask: FILE_GENERIC_READ, sid: other_user() });
            assert!(matches!(check(&user(), &aces, Kind::File, Place::Managed), Err(Refusal::UnknownAce { .. })), "{ace_type:#x}");
        }
    }

    #[test]
    fn sandbox_principals_are_foreign() {
        for sid in [
            all_application_packages(),
            Sid::new(15, &[3, 1]),               // a capability
            Sid::new(4, &[123, 456]),            // Bake's restricted-token capability
            Sid::new(5, &[5, 0, 1234]),          // a logon session
            owner_rights().clone(),
        ] {
            let mut aces = profile();
            aces.push(allow(0, FILE_GENERIC_WRITE, sid.clone()));
            let result = check(&user(), &aces, Kind::File, Place::Managed);
            if sid == owner_rights() {
                assert!(result.is_ok());
            } else {
                assert!(result.is_err(), "{sid}");
            }
        }
    }

    #[test]
    fn integrity_labels() {
        let p = principals();
        for (rid, ok) in [(0x0, false), (0x1000, false), (0x2000, true), (0x3000, true), (0x4000, true)] {
            let sd = descriptor(Some(&user()), Some(Some(&profile())), Some(rid));
            assert_eq!(evaluate(&sd, Kind::File, Place::Managed, &p).is_ok(), ok, "{rid:#x}");
        }
    }

    #[test]
    fn sid_text() {
        assert_eq!(trusted_installer().to_string(), "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464");
        assert_eq!(Sid::new(1 << 40, &[1]).to_string(), "S-1-0x010000000000-1");
    }

    #[test]
    fn malformed_descriptors_are_errors_not_panics() {
        let good = descriptor(Some(&user()), Some(Some(&profile())), Some(0x2000));
        let p = principals();
        for cut in 0..good.len() {
            let _ = evaluate(&good[..cut], Kind::File, Place::Managed, &p);
        }
        // Deterministic xorshift mutations of every byte.
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..20_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let mut bytes = good.clone();
            let at = (state as usize) % bytes.len();
            bytes[at] = (state >> 32) as u8;
            let _ = evaluate(&bytes, Kind::Directory, Place::Ancestor, &p);
        }
        let mut no_self_relative = good.clone();
        no_self_relative[3] = 0;
        assert_eq!(evaluate(&no_self_relative, Kind::File, Place::Managed, &p), Err(Refusal::Malformed("not self-relative")));
        let mut huge_count = good.clone();
        let owner_at = u32::from_le_bytes([good[4], good[5], good[6], good[7]]) as usize;
        huge_count[owner_at + 1] = 16;
        assert!(matches!(evaluate(&huge_count, Kind::File, Place::Managed, &p), Err(Refusal::Malformed(_))));
    }
}
