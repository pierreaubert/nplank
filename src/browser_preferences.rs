//! Transactional browser preferences, independent of localStorage quotas.

// Rust guideline compliant 2026-02-21
use crate::preference_queue::{Queue, Save};
use js_sys::{Promise, Uint8Array};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{Event, IdbDatabase, IdbRequest, IdbTransaction, IdbTransactionMode};

// Version one separates appearance from draft data so mini apps cannot overwrite it.
const DATABASE: &str = "nplank-ui-state";
const STORE: &str = "preferences";

#[derive(Clone, Debug, Default)]
pub(crate) struct Writer(Rc<RefCell<Queue>>);

impl Writer {
    pub fn queue(&self, preferences: Option<String>, appearance: String) {
        if !self.0.borrow_mut().enqueue(Save {
            preferences,
            appearance,
        }) {
            return;
        }
        let state = Rc::clone(&self.0);
        spawn_local(async move {
            loop {
                let Some(save) = state.borrow_mut().next() else {
                    break;
                };
                if let Err(error) = store(save.preferences.as_deref(), &save.appearance).await {
                    state.borrow_mut().failed(save, error);
                    break;
                }
                // This small, optional mirror only synchronizes appearance.
                // Drafts are already committed to IndexedDB at this point.
                if let Some(storage) =
                    web_sys::window().and_then(|window| window.local_storage().ok().flatten())
                {
                    let _ = storage.set_item("nplank.preferences", &save.appearance);
                }
            }
        });
    }

    pub fn take_error(&self) -> Option<String> {
        self.0.borrow_mut().take_error()
    }
}

fn message(error: JsValue) -> String {
    error
        .dyn_ref::<web_sys::DomException>()
        .map(web_sys::DomException::message)
        .or_else(|| error.as_string())
        .unwrap_or_else(|| "Browser storage operation failed".into())
}

struct RequestHandlers {
    request: IdbRequest,
    _success: Closure<dyn FnMut(Event)>,
    _error: Closure<dyn FnMut(Event)>,
}
impl Drop for RequestHandlers {
    fn drop(&mut self) {
        self.request.set_onsuccess(None);
        self.request.set_onerror(None);
    }
}

async fn request(request: &IdbRequest) -> Result<JsValue, String> {
    let mut handlers = None;
    let promise = Promise::new(&mut |resolve, reject| {
        let current = request.clone();
        let failure = reject.clone();
        let success = Closure::<dyn FnMut(Event)>::new(move |_| match current.result() {
            Ok(value) => {
                let _ = resolve.call1(&JsValue::NULL, &value);
            }
            Err(error) => {
                let _ = failure.call1(&JsValue::NULL, &error);
            }
        });
        let current = request.clone();
        let error = Closure::<dyn FnMut(Event)>::new(move |_| {
            let reason = current
                .error()
                .ok()
                .flatten()
                .map(|error| error.message())
                .unwrap_or_else(|| "Browser storage request failed".into());
            let _ = reject.call1(&JsValue::NULL, &JsValue::from_str(&reason));
        });
        request.set_onsuccess(Some(success.as_ref().unchecked_ref()));
        request.set_onerror(Some(error.as_ref().unchecked_ref()));
        handlers = Some(RequestHandlers {
            request: request.clone(),
            _success: success,
            _error: error,
        });
    });
    let result = JsFuture::from(promise).await.map_err(message);
    drop(handlers);
    result
}

struct TransactionHandlers {
    transaction: IdbTransaction,
    _complete: Closure<dyn FnMut(Event)>,
    _abort: Closure<dyn FnMut(Event)>,
    _error: Closure<dyn FnMut(Event)>,
}
impl Drop for TransactionHandlers {
    fn drop(&mut self) {
        self.transaction.set_oncomplete(None);
        self.transaction.set_onabort(None);
        self.transaction.set_onerror(None);
    }
}

async fn committed(transaction: &IdbTransaction) -> Result<(), String> {
    let mut handlers = None;
    let promise = Promise::new(&mut |resolve, reject| {
        let complete = Closure::<dyn FnMut(Event)>::new(move |_| {
            let _ = resolve.call0(&JsValue::NULL);
        });
        let current = transaction.clone();
        let failure = reject.clone();
        let abort = Closure::<dyn FnMut(Event)>::new(move |_| {
            let error = current
                .error()
                .map(|error| error.message())
                .unwrap_or_else(|| "Browser storage transaction aborted".into());
            let _ = failure.call1(&JsValue::NULL, &JsValue::from_str(&error));
        });
        let current = transaction.clone();
        let error = Closure::<dyn FnMut(Event)>::new(move |_| {
            let error = current
                .error()
                .map(|error| error.message())
                .unwrap_or_else(|| "Browser storage transaction failed".into());
            let _ = reject.call1(&JsValue::NULL, &JsValue::from_str(&error));
        });
        transaction.set_oncomplete(Some(complete.as_ref().unchecked_ref()));
        transaction.set_onabort(Some(abort.as_ref().unchecked_ref()));
        transaction.set_onerror(Some(error.as_ref().unchecked_ref()));
        handlers = Some(TransactionHandlers {
            transaction: transaction.clone(),
            _complete: complete,
            _abort: abort,
            _error: error,
        });
    });
    let result = JsFuture::from(promise).await.map(|_| ()).map_err(message);
    drop(handlers);
    result
}

struct Database(IdbDatabase);
impl Drop for Database {
    fn drop(&mut self) {
        self.0.close();
    }
}

struct UpgradeHandler {
    request: web_sys::IdbOpenDbRequest,
    _handler: Closure<dyn FnMut(Event)>,
}
impl Drop for UpgradeHandler {
    fn drop(&mut self) {
        self.request.set_onupgradeneeded(None);
    }
}

async fn open() -> Result<Database, String> {
    let factory = web_sys::window()
        .ok_or("Browser window is unavailable")?
        .indexed_db()
        .map_err(message)?
        .ok_or("IndexedDB is unavailable")?;
    let opening = factory.open_with_u32(DATABASE, 1).map_err(message)?;
    let current = opening.clone();
    let upgrade = Closure::<dyn FnMut(Event)>::new(move |_| {
        if let Ok(value) = current.result()
            && let Ok(database) = value.dyn_into::<IdbDatabase>()
            && !database.object_store_names().contains(STORE)
            && database.create_object_store(STORE).is_err()
            && let Some(transaction) = current.transaction()
        {
            let _ = transaction.abort();
        }
    });
    opening.set_onupgradeneeded(Some(upgrade.as_ref().unchecked_ref()));
    let upgrade = UpgradeHandler {
        request: opening.clone(),
        _handler: upgrade,
    };
    let result = request(opening.unchecked_ref()).await;
    drop(upgrade);
    result?
        .dyn_into::<IdbDatabase>()
        .map(Database)
        .map_err(message)
}

async fn decode(value: JsValue) -> Result<Option<String>, String> {
    if value.is_undefined() {
        return Ok(None);
    }
    if let Some(text) = value.as_string() {
        return Ok(Some(text));
    }
    let blob = value.dyn_into::<web_sys::Blob>().map_err(message)?;
    let buffer = JsFuture::from(blob.array_buffer()).await.map_err(message)?;
    String::from_utf8(Uint8Array::new(&buffer).to_vec())
        .map(Some)
        .map_err(|_| "Browser preferences contain invalid UTF-8".into())
}

/// Reads draft and appearance records from one transaction snapshot.
pub(crate) async fn load() -> Result<(Option<String>, Option<String>), String> {
    let database = open().await?;
    let transaction = database
        .0
        .transaction_with_str_and_mode(STORE, IdbTransactionMode::Readonly)
        .map_err(message)?;
    let store = transaction.object_store(STORE).map_err(message)?;
    let preferences = store.get(&JsValue::from_str("drafts")).map_err(message)?;
    let appearance = store
        .get(&JsValue::from_str("appearance"))
        .map_err(message)?;
    let (preferences, appearance) =
        futures_util::join!(request(&preferences), request(&appearance));
    Ok((decode(preferences?).await?, decode(appearance?).await?))
}

/// Commits drafts and appearance atomically, or updates appearance alone.
pub(crate) async fn store(preferences: Option<&str>, appearance: &str) -> Result<(), String> {
    let database = open().await?;
    let transaction = database
        .0
        .transaction_with_str_and_mode(STORE, IdbTransactionMode::Readwrite)
        .map_err(message)?;
    let store = transaction.object_store(STORE).map_err(message)?;
    let queued = (|| -> Result<(), String> {
        if let Some(text) = preferences {
            let bytes = Uint8Array::from(text.as_bytes());
            let parts = js_sys::Array::new();
            parts.push(&bytes);
            let blob = web_sys::Blob::new_with_u8_array_sequence(&parts).map_err(message)?;
            store
                .put_with_key(&blob, &JsValue::from_str("drafts"))
                .map_err(message)?;
        }
        store
            .put_with_key(
                &JsValue::from_str(appearance),
                &JsValue::from_str("appearance"),
            )
            .map_err(message)?;
        Ok(())
    })();
    if let Err(error) = queued {
        let _ = transaction.abort();
        return Err(error);
    }
    committed(&transaction).await
}
