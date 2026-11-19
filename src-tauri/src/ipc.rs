//! IPC streaming `Channel` abstraction.
//!
//! Desktop builds re-export `tauri::ipc::*` directly so `#[tauri::command]`
//! functions take Tauri's real `Channel<T>` without conversion. Headless CLI
//! builds (`--no-default-features`, i.e. `vicuna serve`) compile without `tauri`
//! at all — dropping GTK/WebKit/pkg-config so Linux binaries build statically
//! against musl — and use this drop-in in-process callback shim instead.

#[cfg(feature = "desktop")]
pub use tauri::ipc::{Channel, InvokeResponseBody, IpcResponse};

#[cfg(feature = "desktop")]
pub type IpcError = tauri::Error;

#[cfg(feature = "desktop")]
pub fn io_error(e: std::io::Error) -> IpcError {
    tauri::Error::Io(e)
}

#[cfg(not(feature = "desktop"))]
mod shim {
    use std::marker::PhantomData;
    use std::sync::Arc;

    #[derive(Debug)]
    pub enum IpcError {
        Json(serde_json::Error),
        Io(std::io::Error),
    }

    impl std::fmt::Display for IpcError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Json(e) => write!(f, "{e}"),
                Self::Io(e) => write!(f, "{e}"),
            }
        }
    }

    impl std::error::Error for IpcError {}

    pub fn io_error(e: std::io::Error) -> IpcError {
        IpcError::Io(e)
    }

    #[derive(Debug, Clone)]
    pub enum InvokeResponseBody {
        Json(String),
        #[allow(dead_code)]
        Raw(Vec<u8>),
    }

    pub trait IpcResponse {
        fn body(self) -> Result<InvokeResponseBody, IpcError>;
    }

    impl<T: serde::Serialize> IpcResponse for T {
        fn body(self) -> Result<InvokeResponseBody, IpcError> {
            serde_json::to_string(&self)
                .map(InvokeResponseBody::Json)
                .map_err(IpcError::Json)
        }
    }

    type OnMessage = Arc<dyn Fn(InvokeResponseBody) -> Result<(), IpcError> + Send + Sync>;

    pub struct Channel<T = InvokeResponseBody> {
        on_message: OnMessage,
        _marker: PhantomData<fn(T)>,
    }

    impl<T> Clone for Channel<T> {
        fn clone(&self) -> Self {
            Self {
                on_message: Arc::clone(&self.on_message),
                _marker: PhantomData,
            }
        }
    }

    impl<T: IpcResponse> Channel<T> {
        pub fn new<F>(on_message: F) -> Self
        where
            F: Fn(InvokeResponseBody) -> Result<(), IpcError> + Send + Sync + 'static,
        {
            Self {
                on_message: Arc::new(on_message),
                _marker: PhantomData,
            }
        }

        pub fn send(&self, data: T) -> Result<(), IpcError> {
            let body = data.body()?;
            (self.on_message)(body)
        }
    }
}

#[cfg(not(feature = "desktop"))]
pub use shim::{io_error, Channel, InvokeResponseBody, IpcError, IpcResponse};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn channel_sends_json_and_clones_handler() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen2 = seen.clone();
        let ch = Channel::<serde_json::Value>::new(move |body| {
            if let InvokeResponseBody::Json(s) = body {
                seen2.lock().unwrap().push(s);
            }
            Ok(())
        });
        let ch2 = ch.clone();
        ch.send(serde_json::json!({"n": 1})).unwrap();
        ch2.send(serde_json::json!({"n": 2})).unwrap();
        let out = seen.lock().unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&out[0]).unwrap()["n"], 1);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&out[1]).unwrap()["n"], 2);
    }
}
