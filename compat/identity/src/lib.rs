//! Manifest-derived identity shared by the launcher and its in-process shim.
use anyhow::{bail, ensure, Context, Result};
use quick_xml::{events::Event, Reader};
use std::path::Path;
use windows_sys::Win32::Storage::Packaging::Appx::{
    PackageFamilyNameFromId, PackageFullNameFromId, PACKAGE_ID,
};

pub fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

pub struct Identity {
    pub name: Vec<u16>,
    pub publisher: Vec<u16>,
    pub resource: Vec<u16>,
    pub publisher_id: Vec<u16>,
    pub version: u64,
    pub full: Vec<u16>,
    pub family: Vec<u16>,
}

impl Identity {
    pub fn from_manifest(path: &Path) -> Result<Self> {
        let xml =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&xml)
    }

    pub fn parse(xml: &str) -> Result<Self> {
        let mut reader = Reader::from_str(xml);
        loop {
            match reader.read_event()? {
                Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"Identity" => {
                    let attrs = e
                        .attributes()
                        .map(|a| {
                            let a = a?;
                            Ok((
                                String::from_utf8(a.key.as_ref().to_vec())?,
                                a.unescape_value()?.into_owned(),
                            ))
                        })
                        .collect::<Result<std::collections::HashMap<String, String>>>()?;
                    let get = |key: &str| {
                        attrs
                            .get(key)
                            .with_context(|| format!("manifest Identity missing {key}"))
                    };
                    ensure!(
                        get("ProcessorArchitecture")? == "x64",
                        "identity shim currently supports x64 packages only"
                    );
                    let parts = get("Version")?
                        .split('.')
                        .map(str::parse::<u16>)
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    ensure!(
                        parts.len() == 4,
                        "package version needs four u16 components"
                    );
                    let mut identity = Self {
                        name: wide(get("Name")?),
                        publisher: wide(get("Publisher")?),
                        resource: wide(attrs.get("ResourceId").map(String::as_str).unwrap_or("")),
                        publisher_id: vec![0],
                        version: parts.iter().fold(0, |v, p| (v << 16) | *p as u64),
                        full: vec![],
                        family: vec![],
                    };
                    // Windows computes the publisher ID from the actual publisher DN.
                    let mut id = identity.package_id();
                    id.publisherId = std::ptr::null_mut();
                    identity.full = query_name(&id, PackageFullNameFromId)?;
                    identity.family = query_name(&id, PackageFamilyNameFromId)?;
                    let family = String::from_utf16(&identity.family[..identity.family.len() - 1])?;
                    identity.publisher_id = wide(
                        family
                            .rsplit_once('_')
                            .context("invalid package family name")?
                            .1,
                    );
                    return Ok(identity);
                }
                Event::Eof => bail!("manifest contains no Identity element"),
                _ => {}
            }
        }
    }

    pub fn package_id(&self) -> PACKAGE_ID {
        let mut id: PACKAGE_ID = unsafe { std::mem::zeroed() };
        id.processorArchitecture = 9;
        id.version.Anonymous.Version = self.version;
        id.name = self.name.as_ptr() as _;
        id.publisher = self.publisher.as_ptr() as _;
        id.resourceId = self.resource.as_ptr() as _;
        id.publisherId = self.publisher_id.as_ptr() as _;
        id
    }

    /// Implements the byte-counted PACKAGE_ID ABI, with all pointers in the caller's buffer.
    ///
    /// # Safety
    /// `len` must point to a writable u32 and `out`, if non-null, must have `*len` writable bytes.
    pub unsafe fn write_id(&self, len: *mut u32, out: *mut u8) -> i32 {
        if len.is_null() || (out.is_null() && *len != 0) {
            return 87;
        }
        let strings = [
            &self.name,
            &self.publisher,
            &self.resource,
            &self.publisher_id,
        ];
        let need =
            std::mem::size_of::<PACKAGE_ID>() + strings.iter().map(|s| s.len() * 2).sum::<usize>();
        let capacity = *len as usize;
        *len = need as u32;
        if out.is_null() || capacity < need {
            return 122;
        }
        let mut id = self.package_id();
        let mut cursor = out.add(std::mem::size_of::<PACKAGE_ID>());
        for (s, slot) in strings.into_iter().zip([
            std::ptr::addr_of_mut!(id.name),
            std::ptr::addr_of_mut!(id.publisher),
            std::ptr::addr_of_mut!(id.resourceId),
            std::ptr::addr_of_mut!(id.publisherId),
        ]) {
            std::ptr::write_unaligned(slot, cursor as _);
            std::ptr::copy_nonoverlapping(s.as_ptr() as *const u8, cursor, s.len() * 2);
            cursor = cursor.add(s.len() * 2);
        }
        std::ptr::write_unaligned(out as *mut PACKAGE_ID, id);
        0
    }
}

fn query_name(
    id: &PACKAGE_ID,
    api: unsafe extern "system" fn(*const PACKAGE_ID, *mut u32, *mut u16) -> u32,
) -> Result<Vec<u16>> {
    let mut len = 0;
    let code = unsafe { api(id, &mut len, std::ptr::null_mut()) };
    ensure!(code == 122, "package name size query returned {code}");
    let mut result = vec![0; len as usize];
    let code = unsafe { api(id, &mut len, result.as_mut_ptr()) };
    ensure!(code == 0, "package name query returned {code}");
    Ok(result)
}

/// # Safety
/// `len` and `out` must satisfy the Win32 character-counted output buffer contract.
pub unsafe fn write_string(value: &[u16], len: *mut u32, out: *mut u16) -> i32 {
    if len.is_null() || (out.is_null() && *len != 0) {
        return 87;
    }
    let capacity = *len as usize;
    *len = value.len() as u32;
    if out.is_null() || capacity < value.len() {
        return 122;
    }
    std::ptr::copy_nonoverlapping(value.as_ptr(), out, value.len());
    0
}
