//! HTTP and WebSocket limited to this computer, for SteelSeries GG, Sonar and GameSense.
//!
//! The host is not a parameter and no proxy is used, so nothing here can reach another machine.

use std::ffi::c_void;
use std::mem;
use std::ptr;

use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Networking::WinHttp::{
    SECURITY_FLAG_IGNORE_CERT_CN_INVALID, SECURITY_FLAG_IGNORE_CERT_DATE_INVALID,
    SECURITY_FLAG_IGNORE_CERT_WRONG_USAGE, SECURITY_FLAG_IGNORE_UNKNOWN_CA,
    WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
    WINHTTP_OPTION_SECURITY_FLAGS, WINHTTP_OPTION_UPGRADE_TO_WEB_SOCKET, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WINHTTP_WEB_SOCKET_SUCCESS_CLOSE_STATUS,
    WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen,
    WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts, WinHttpWebSocketClose,
    WinHttpWebSocketCompleteUpgrade, WinHttpWebSocketSend,
};
use windows::core::{Error, HSTRING, PCWSTR, Result, w};

const TIMEOUT_MS: i32 = 2000;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

pub enum Scheme {
    Http,
    Https,
}

struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void) -> Result<Self> {
        if raw.is_null() {
            Err(Error::from_thread())
        } else {
            Ok(Self(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a valid WinHTTP handle, closed only here.
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

/// An open request. Fields drop in order, so the handles close from the innermost out.
struct Exchange {
    request: Handle,
    _connection: Handle,
    _session: Handle,
}

fn open(method: &str, scheme: Scheme, port: u16, path: &str) -> Result<Exchange> {
    let method = HSTRING::from(method);
    let path = HSTRING::from(path);
    let secure = matches!(scheme, Scheme::Https);
    let flags = if secure {
        WINHTTP_FLAG_SECURE
    } else {
        WINHTTP_OPEN_REQUEST_FLAGS(0)
    };
    // SAFETY: each handle is checked before use and the strings outlive these calls.
    unsafe {
        let session = Handle::new(WinHttpOpen(
            w!("sonar-rollerbar"),
            WINHTTP_ACCESS_TYPE_NO_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ))?;
        WinHttpSetTimeouts(session.0, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS)?;
        let connection = Handle::new(WinHttpConnect(session.0, w!("127.0.0.1"), port, 0))?;
        let request = Handle::new(WinHttpOpenRequest(
            connection.0,
            &method,
            &path,
            PCWSTR::null(),
            PCWSTR::null(),
            ptr::null(),
            flags,
        ))?;
        if secure {
            // GG serves its local HTTPS with a self-signed certificate.
            let ignored = SECURITY_FLAG_IGNORE_UNKNOWN_CA
                | SECURITY_FLAG_IGNORE_CERT_CN_INVALID
                | SECURITY_FLAG_IGNORE_CERT_DATE_INVALID
                | SECURITY_FLAG_IGNORE_CERT_WRONG_USAGE;
            WinHttpSetOption(
                Some(request.0),
                WINHTTP_OPTION_SECURITY_FLAGS,
                Some(&ignored.to_ne_bytes()),
            )?;
        }
        Ok(Exchange {
            request,
            _connection: connection,
            _session: session,
        })
    }
}

fn status_code(request: &Handle) -> Result<u32> {
    let mut status = 0u32;
    let mut size = mem::size_of::<u32>() as u32;
    // SAFETY: the buffer and its size describe the local `status`.
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(ptr::from_mut(&mut status).cast()),
            &mut size,
            ptr::null_mut(),
        )?;
    }
    Ok(status)
}

fn read_body(request: &Handle) -> Result<String> {
    let mut body = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let mut read = 0u32;
        // SAFETY: the pointer and length describe the local `buffer`.
        unsafe {
            WinHttpReadData(
                request.0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
            )?;
        }
        if read == 0 {
            return Ok(String::from_utf8_lossy(&body).into_owned());
        }
        body.extend_from_slice(&buffer[..read as usize]);
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(Error::new(E_FAIL, "HTTP response too large"));
        }
    }
}

/// Sends a request to 127.0.0.1, with an optional JSON body, and returns the response body.
pub fn request(
    method: &str,
    scheme: Scheme,
    port: u16,
    path: &str,
    json_body: Option<&str>,
) -> Result<String> {
    let exchange = open(method, scheme, port, path)?;
    let request = &exchange.request;
    let headers: Vec<u16> = "Content-Type: application/json".encode_utf16().collect();
    let body = json_body.unwrap_or_default();
    let length = body.len() as u32;
    // SAFETY: the headers and body outlive the call, and `length` is the body's size in bytes.
    unsafe {
        WinHttpSendRequest(
            request.0,
            json_body.map(|_| headers.as_slice()),
            json_body.map(|_| body.as_ptr().cast()),
            length,
            length,
            0,
        )?;
        WinHttpReceiveResponse(request.0, ptr::null_mut())?;
    }
    let status = status_code(request)?;
    if !(200..300).contains(&status) {
        return Err(Error::new(E_FAIL, format!("HTTP status {status}")));
    }
    read_body(request)
}

/// Opens a secure WebSocket to 127.0.0.1, sends one text message and closes it.
pub fn websocket_send(port: u16, path: &str, message: &str) -> Result<()> {
    let exchange = open("GET", Scheme::Https, port, path)?;
    let request = &exchange.request;
    // SAFETY: the request is valid and the upgrade option takes no value.
    let socket = unsafe {
        WinHttpSetOption(Some(request.0), WINHTTP_OPTION_UPGRADE_TO_WEB_SOCKET, None)?;
        WinHttpSendRequest(request.0, None, None, 0, 0, 0)?;
        WinHttpReceiveResponse(request.0, ptr::null_mut())?;
        Handle::new(WinHttpWebSocketCompleteUpgrade(request.0, None))?
    };
    // SAFETY: `socket` is an upgraded WebSocket handle and `message` outlives the calls.
    let error = unsafe {
        let error = WinHttpWebSocketSend(
            socket.0,
            WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE,
            Some(message.as_bytes()),
        );
        let status = WINHTTP_WEB_SOCKET_SUCCESS_CLOSE_STATUS.0 as u16;
        let _ = WinHttpWebSocketClose(socket.0, status, None, 0);
        error
    };
    if error == 0 {
        Ok(())
    } else {
        Err(Error::new(E_FAIL, format!("WebSocket error {error}")))
    }
}
