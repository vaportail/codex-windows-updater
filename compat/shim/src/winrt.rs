//! Minimal read-only Package.Current facade for the native updater's identity queries.
//! Only the addon's RoGetActivationFactory import is redirected; other callers use Windows.
use std::{ffi::c_void, mem::MaybeUninit, ptr::null_mut};
use windows::{
    core::{IInspectable_Vtbl, IUnknown_Vtbl, Interface, GUID, HRESULT, HSTRING},
    ApplicationModel::{
        IPackage, IPackageId, IPackageId_Vtbl, IPackageStatics, IPackageStatics_Vtbl,
        IPackage_Vtbl, PackageVersion,
    },
    System::ProcessorArchitecture,
};
use windows_sys::Win32::System::Com::CoTaskMemAlloc;
#[link(name = "runtimeobject")]
extern "system" {
    fn RoGetActivationFactory(class: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> i32;
    fn WindowsGetStringRawBuffer(value: *mut c_void, length: *mut u32) -> *const u16;
}

const OK: HRESULT = HRESULT(0);
const POINTER: HRESULT = HRESULT(0x80004003u32 as i32);
const NO_INTERFACE: HRESULT = HRESULT(0x80004002u32 as i32);
const NOT_IMPL: HRESULT = HRESULT(0x80004001u32 as i32);
const IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IINSPECTABLE: GUID = GUID::from_u128(0xaf86e2e0_b12d_4c6a_9c5a_d7aa65101e90);
const IAGILE: GUID = GUID::from_u128(0x94ea2b94_e9cc_49e0_c0ff_ee64ca8f5b90);

#[repr(C)]
struct Object<V: 'static> {
    vtable: &'static V,
    iid: GUID,
}
// Objects and their manifest data live for the process lifetime. Refcounts are immortal.
unsafe extern "system" fn query(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if iid.is_null() || out.is_null() {
        return POINTER;
    }
    *out = null_mut();
    let own = (*(this as *const Object<IInspectable_Vtbl>)).iid;
    if *iid == own || *iid == IUNKNOWN || *iid == IINSPECTABLE || *iid == IAGILE {
        *out = this;
        OK
    } else {
        NO_INTERFACE
    }
}
unsafe extern "system" fn add_ref(_: *mut c_void) -> u32 {
    2
}
unsafe extern "system" fn release(_: *mut c_void) -> u32 {
    1
}
unsafe extern "system" fn iids(this: *mut c_void, count: *mut u32, out: *mut *mut GUID) -> HRESULT {
    if count.is_null() || out.is_null() {
        return POINTER;
    }
    *count = 0;
    *out = null_mut();
    let buffer = CoTaskMemAlloc(std::mem::size_of::<GUID>()) as *mut GUID;
    if buffer.is_null() {
        return HRESULT(0x8007000eu32 as i32);
    }
    *buffer = (*(this as *const Object<IInspectable_Vtbl>)).iid;
    *out = buffer;
    *count = 1;
    OK
}
unsafe extern "system" fn class_name(this: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() {
        return POINTER;
    }
    let own = (*(this as *const Object<IInspectable_Vtbl>)).iid;
    std::ptr::write(
        out as *mut HSTRING,
        HSTRING::from(if own == IPackageId::IID {
            "Windows.ApplicationModel.PackageId"
        } else {
            "Windows.ApplicationModel.Package"
        }),
    );
    OK
}
unsafe extern "system" fn trust(_: *mut c_void, out: *mut i32) -> HRESULT {
    if out.is_null() {
        return POINTER;
    }
    *out = 0;
    OK
}
const BASE: IInspectable_Vtbl = IInspectable_Vtbl {
    base: IUnknown_Vtbl {
        QueryInterface: query,
        AddRef: add_ref,
        Release: release,
    },
    GetIids: iids,
    GetRuntimeClassName: class_name,
    GetTrustLevel: trust,
};
unsafe fn output_object<T>(out: *mut *mut c_void, object: &'static Object<T>) -> HRESULT {
    if out.is_null() {
        return POINTER;
    }
    *out = object as *const _ as *mut c_void;
    OK
}
unsafe extern "system" fn current(_: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    super::trace(b"WinRT Package.Current\n");
    output_object(out, &PACKAGE)
}
unsafe extern "system" fn id(_: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    output_object(out, &IDENTIFIER)
}
unsafe extern "system" fn unavailable(_: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() {
        return POINTER;
    }
    *out = null_mut();
    NOT_IMPL
}
unsafe extern "system" fn is_framework(_: *mut c_void, out: *mut bool) -> HRESULT {
    if out.is_null() {
        return POINTER;
    }
    *out = false;
    OK
}
unsafe fn string(value: &[u16], out: *mut MaybeUninit<HSTRING>) -> HRESULT {
    if out.is_null() {
        return POINTER;
    }
    match HSTRING::from_wide(&value[..value.len() - 1]) {
        Ok(value) => {
            (*out).write(value);
            OK
        }
        Err(error) => error.code(),
    }
}
macro_rules! string_getter {
    ($function:ident, $field:ident) => {
        unsafe extern "system" fn $function(
            _: *mut c_void,
            out: *mut MaybeUninit<HSTRING>,
        ) -> HRESULT {
            string(&super::IDENTITY.get().unwrap().$field, out)
        }
    };
}
string_getter!(name, name);
string_getter!(resource, resource);
string_getter!(publisher, publisher);
string_getter!(publisher_id, publisher_id);
string_getter!(full, full);
string_getter!(family, family);
unsafe extern "system" fn version(_: *mut c_void, out: *mut PackageVersion) -> HRESULT {
    if out.is_null() {
        return POINTER;
    }
    let v = super::IDENTITY.get().unwrap().version;
    *out = PackageVersion {
        Major: (v >> 48) as u16,
        Minor: (v >> 32) as u16,
        Build: (v >> 16) as u16,
        Revision: v as u16,
    };
    OK
}
unsafe extern "system" fn architecture(_: *mut c_void, out: *mut ProcessorArchitecture) -> HRESULT {
    if out.is_null() {
        return POINTER;
    }
    *out = ProcessorArchitecture::X64;
    OK
}
static FACTORY_VTABLE: IPackageStatics_Vtbl = IPackageStatics_Vtbl {
    base__: BASE,
    Current: current,
};
static PACKAGE_VTABLE: IPackage_Vtbl = IPackage_Vtbl {
    base__: BASE,
    Id: id,
    InstalledLocation: unavailable,
    IsFramework: is_framework,
    Dependencies: unavailable,
};
static ID_VTABLE: IPackageId_Vtbl = IPackageId_Vtbl {
    base__: BASE,
    Name: name,
    Version: version,
    Architecture: architecture,
    ResourceId: resource,
    Publisher: publisher,
    PublisherId: publisher_id,
    FullName: full,
    FamilyName: family,
};
static FACTORY: Object<IPackageStatics_Vtbl> = Object {
    vtable: &FACTORY_VTABLE,
    iid: IPackageStatics::IID,
};
static PACKAGE: Object<IPackage_Vtbl> = Object {
    vtable: &PACKAGE_VTABLE,
    iid: IPackage::IID,
};
static IDENTIFIER: Object<IPackageId_Vtbl> = Object {
    vtable: &ID_VTABLE,
    iid: IPackageId::IID,
};

pub unsafe extern "system" fn activation_factory(
    class: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if iid.is_null() || out.is_null() {
        return POINTER;
    }
    let mut len = 0;
    let raw = WindowsGetStringRawBuffer(class, &mut len);
    let is_package = !raw.is_null()
        && std::slice::from_raw_parts(raw, len as usize)
            .iter()
            .copied()
            .eq("Windows.ApplicationModel.Package".encode_utf16());
    if is_package && *iid == IPackageStatics::IID {
        output_object(out, &FACTORY)
    } else {
        HRESULT(RoGetActivationFactory(class, iid as _, out))
    }
}
