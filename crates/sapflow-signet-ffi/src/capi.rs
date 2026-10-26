#![allow(unsafe_code)]
#![allow(unsafe_op_in_unsafe_fn)]

use crate::{NativeSignet, SignetCredential, SignetError, SignetEvent};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::slice;
use std::sync::Arc;
use zeroize::Zeroize;

pub const ABI_VERSION: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SapflowSignetBuffer {
    pub ptr: *mut u8,
    pub len: usize,
}

#[repr(C)]
#[derive(Default)]
pub struct SapflowSignetEvent {
    pub tag: u32,
    pub request_id: u64,
    pub error: i32,
    pub bytes: SapflowSignetBuffer,
    pub credential: *mut c_void,
}

fn code(error: SignetError) -> i32 {
    match error {
        SignetError::InvalidInput => 1,
        SignetError::InvalidCredential => 2,
        SignetError::InvalidProof => 3,
        SignetError::Busy => 4,
        SignetError::Deadline => 5,
        SignetError::Cancelled => 6,
        SignetError::Refused => 7,
        SignetError::Unavailable => 8,
        SignetError::Closed => 9,
    }
}

fn argument() -> SignetError {
    SignetError::InvalidInput
}

fn owned(bytes: Vec<u8>) -> SapflowSignetBuffer {
    let mut bytes = bytes.into_boxed_slice();
    let out = SapflowSignetBuffer {
        ptr: bytes.as_mut_ptr(),
        len: bytes.len(),
    };
    std::mem::forget(bytes);
    out
}

unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> Result<&'a [u8], SignetError> {
    if ptr.is_null() {
        if len == 0 { Ok(&[]) } else { Err(argument()) }
    } else {
        Ok(slice::from_raw_parts(ptr, len))
    }
}

unsafe fn text(ptr: *const u8, len: usize) -> Result<String, SignetError> {
    std::str::from_utf8(bytes(ptr, len)?)
        .map(str::to_owned)
        .map_err(|_| argument())
}

unsafe fn object<'a, T>(value: *const c_void) -> Result<&'a T, SignetError> {
    value.cast::<T>().as_ref().ok_or_else(argument)
}

unsafe fn clone_handle<T>(value: *const c_void) -> Result<Arc<T>, SignetError> {
    let ptr = value.cast::<T>();
    if ptr.is_null() {
        return Err(argument());
    }
    Arc::increment_strong_count(ptr);
    Ok(Arc::from_raw(ptr))
}

fn handle<T>(value: Arc<T>) -> *mut c_void {
    Arc::into_raw(value).cast_mut().cast()
}

unsafe fn write<T>(out: *mut T, call: impl FnOnce() -> Result<T, SignetError>) -> i32 {
    if out.is_null() {
        return code(argument());
    }
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(Ok(value)) => {
            out.write(value);
            0
        }
        Ok(Err(error)) => code(error),
        Err(_) => code(SignetError::Unavailable),
    }
}

unsafe fn unit(call: impl FnOnce() -> Result<(), SignetError>) -> i32 {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => code(error),
        Err(_) => code(SignetError::Unavailable),
    }
}

unsafe fn free_handle<T>(value: *mut c_void) {
    if !value.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            drop(Arc::from_raw(value.cast::<T>()));
        }));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn sapflow_signet_abi_version() -> u32 {
    ABI_VERSION
}

#[unsafe(no_mangle)]
pub extern "C" fn sapflow_signet_buffer_size() -> usize {
    std::mem::size_of::<SapflowSignetBuffer>()
}

#[unsafe(no_mangle)]
pub extern "C" fn sapflow_signet_event_size() -> usize {
    std::mem::size_of::<SapflowSignetEvent>()
}

#[unsafe(no_mangle)]
pub extern "C" fn sapflow_signet_buffer_free(buffer: SapflowSignetBuffer) {
    if !buffer.ptr.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                buffer.ptr, buffer.len,
            )));
        }));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn sapflow_signet_secret_free(buffer: SapflowSignetBuffer) {
    if !buffer.ptr.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            slice::from_raw_parts_mut(buffer.ptr, buffer.len).zeroize();
        }));
    }
    sapflow_signet_buffer_free(buffer);
}

#[unsafe(no_mangle)]
pub extern "C" fn sapflow_signet_event_free(event: SapflowSignetEvent) {
    sapflow_signet_buffer_free(event.bytes);
    unsafe { free_handle::<SignetCredential>(event.credential) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_signet_credential_restore(
    input: *const u8,
    input_len: usize,
    out: *mut *mut c_void,
) -> i32 {
    write(out, || {
        Ok(handle(SignetCredential::restore(
            bytes(input, input_len)?.to_vec(),
        )?))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_signet_credential_free(credential: *mut c_void) {
    free_handle::<SignetCredential>(credential);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_signet_credential_encoded(
    credential: *const c_void,
    out: *mut SapflowSignetBuffer,
) -> i32 {
    write(out, || {
        Ok(owned(object::<SignetCredential>(credential)?.encoded()))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_signet_native_pair_bunker(
    uri: *const u8,
    uri_len: usize,
    deadline_ms: u32,
    out: *mut *mut c_void,
) -> i32 {
    write(out, || {
        Ok(handle(NativeSignet::pair_bunker(
            text(uri, uri_len)?,
            deadline_ms,
        )?))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_signet_native_restore(
    credential: *const c_void,
    deadline_ms: u32,
    out: *mut *mut c_void,
) -> i32 {
    write(out, || {
        Ok(handle(NativeSignet::restore(
            clone_handle::<SignetCredential>(credential)?,
            deadline_ms,
        )?))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_signet_native_free(owner: *mut c_void) {
    free_handle::<NativeSignet>(owner);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_signet_native_connect(
    owner: *const c_void,
    request_id: u64,
) -> i32 {
    unit(|| object::<NativeSignet>(owner)?.connect(request_id))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_signet_native_sign_login(
    owner: *const c_void,
    request_id: u64,
    challenge: *const u8,
    challenge_len: usize,
    origin: *const u8,
    origin_len: usize,
    app_name: *const u8,
    app_name_len: usize,
    created_at_seconds: u64,
) -> i32 {
    unit(|| {
        object::<NativeSignet>(owner)?.sign_login(
            request_id,
            text(challenge, challenge_len)?,
            text(origin, origin_len)?,
            text(app_name, app_name_len)?,
            created_at_seconds,
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_signet_native_wait_event(
    owner: *const c_void,
    timeout_ms: u32,
    out: *mut SapflowSignetEvent,
) -> i32 {
    write(out, || {
        Ok(
            match object::<NativeSignet>(owner)?.wait_event(timeout_ms)? {
                None => SapflowSignetEvent::default(),
                Some(SignetEvent::Ready {
                    request_id,
                    persona,
                    credential,
                }) => SapflowSignetEvent {
                    tag: 1,
                    request_id,
                    error: 0,
                    bytes: owned(persona.into_bytes()),
                    credential: handle(credential),
                },
                Some(SignetEvent::Proof { request_id, json }) => SapflowSignetEvent {
                    tag: 2,
                    request_id,
                    error: 0,
                    bytes: owned(json),
                    credential: std::ptr::null_mut(),
                },
                Some(SignetEvent::Failed { request_id, error }) => SapflowSignetEvent {
                    tag: 3,
                    request_id,
                    error: code(error),
                    bytes: SapflowSignetBuffer::default(),
                    credential: std::ptr::null_mut(),
                },
            },
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_signet_native_request_close(owner: *const c_void) -> i32 {
    unit(|| {
        object::<NativeSignet>(owner)?.request_close();
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_signet_native_wait_closed(
    owner: *const c_void,
    timeout_ms: u32,
    out: *mut bool,
) -> i32 {
    write(out, || {
        object::<NativeSignet>(owner)?.wait_closed(timeout_ms)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sapflow_signet_native_is_closed(
    owner: *const c_void,
    out: *mut bool,
) -> i32 {
    write(out, || Ok(object::<NativeSignet>(owner)?.is_closed()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERSONA: &str = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
    const URI: &str = "bunker://79dff8f82963424e0bb02708a22e44b4980893e3a4be0fa3cb60a43b946764e3?relay=wss://relay.example.invalid";

    fn credential() -> Vec<u8> {
        format!(
            "{{\"version\":1,\"persona\":\"{PERSONA}\",\"app_secret_key\":\"{}\",\"bunker_uri\":\"{URI}\"}}",
            "1".repeat(64)
        )
        .into_bytes()
    }

    #[test]
    fn credential_buffers_are_copies_and_errors_are_fixed() {
        unsafe {
            assert_eq!(sapflow_signet_abi_version(), ABI_VERSION);
            assert_eq!(
                sapflow_signet_buffer_size(),
                std::mem::size_of::<SapflowSignetBuffer>()
            );
            assert_eq!(
                sapflow_signet_event_size(),
                std::mem::size_of::<SapflowSignetEvent>()
            );
            let expected = credential();
            let mut input = expected.clone();
            let mut handle = std::ptr::null_mut();
            assert_eq!(
                sapflow_signet_credential_restore(input.as_ptr(), input.len(), &mut handle),
                0
            );
            input.zeroize();
            let mut encoded = SapflowSignetBuffer::default();
            assert_eq!(sapflow_signet_credential_encoded(handle, &mut encoded), 0);
            assert_eq!(bytes(encoded.ptr, encoded.len).expect("output"), expected);
            sapflow_signet_secret_free(encoded);
            sapflow_signet_credential_free(handle);
            handle = std::ptr::null_mut();

            let bad = b"{}";
            assert_eq!(
                sapflow_signet_credential_restore(bad.as_ptr(), bad.len(), &mut handle),
                code(SignetError::InvalidCredential)
            );
            assert!(handle.is_null());
            assert_eq!(
                sapflow_signet_credential_restore(std::ptr::null(), 1, &mut handle),
                code(SignetError::InvalidInput)
            );
        }
    }

    unsafe fn next_event(owner: *const c_void) -> SapflowSignetEvent {
        for _ in 0..20 {
            let mut event = SapflowSignetEvent::default();
            assert_eq!(sapflow_signet_native_wait_event(owner, 100, &mut event), 0);
            if event.tag != 0 {
                return event;
            }
        }
        panic!("event deadline");
    }

    #[test]
    fn async_ready_and_proof_cross_the_c_abi() {
        use nostr::prelude::*;

        let _serial = crate::owner::tests::SERIAL.lock().expect("serial");
        unsafe {
            let owner = handle(crate::owner::tests::capi_owner());
            assert_eq!(sapflow_signet_native_connect(owner, 1), 0);
            let ready = next_event(owner);
            assert_eq!(ready.tag, 1);
            assert_eq!(ready.request_id, 1);
            assert!(!ready.credential.is_null());
            assert_eq!(ready.bytes.len, 64);
            let mut encoded = SapflowSignetBuffer::default();
            assert_eq!(
                sapflow_signet_credential_encoded(ready.credential, &mut encoded),
                0
            );
            let mut restored = std::ptr::null_mut();
            assert_eq!(
                sapflow_signet_credential_restore(encoded.ptr, encoded.len, &mut restored),
                0
            );
            sapflow_signet_secret_free(encoded);
            sapflow_signet_credential_free(restored);
            sapflow_signet_event_free(ready);

            let created_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_secs();
            let challenge = "a".repeat(64);
            let origin = "https://api.example.invalid";
            let app = "Generated test";
            assert_eq!(
                sapflow_signet_native_sign_login(
                    owner,
                    2,
                    challenge.as_ptr(),
                    challenge.len(),
                    origin.as_ptr(),
                    origin.len(),
                    app.as_ptr(),
                    app.len(),
                    created_at,
                ),
                0
            );
            let proof = next_event(owner);
            assert_eq!(proof.tag, 2);
            assert_eq!(proof.request_id, 2);
            let event = Event::from_json(bytes(proof.bytes.ptr, proof.bytes.len).expect("JSON"))
                .expect("signed event");
            assert_eq!(event.kind, Kind::Custom(21_236));
            assert!(event.verify().is_ok());
            sapflow_signet_event_free(proof);

            assert_eq!(sapflow_signet_native_request_close(owner), 0);
            let mut closed = false;
            assert_eq!(
                sapflow_signet_native_wait_closed(owner, 2_000, &mut closed),
                0
            );
            assert!(closed);
            sapflow_signet_native_free(owner);
        }
    }
}
