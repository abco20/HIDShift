use std::rc::Rc;

use gloo_timers::future::TimeoutFuture;
use hidshift::{
    MANAGEMENT_EVENT_LEN, MANAGEMENT_EVENT_UUID, MANAGEMENT_HID_REQUEST_REPORT_ID,
    MANAGEMENT_HID_USAGE, MANAGEMENT_HID_USAGE_PAGE, MANAGEMENT_REQUEST_UUID,
    MANAGEMENT_RESPONSE_LEN, MANAGEMENT_RESPONSE_UUID, MANAGEMENT_SERVICE_UUID,
};
use hidshift_client::{HidManagementFrame, PendingRequest, decode_hid_input, encode_hid_request};
use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Event, EventTarget};

type BytesCallback = Rc<dyn Fn(&[u8])>;
type DisconnectCallback = Rc<dyn Fn(String)>;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NativeCompanionSnapshot {
    pub revision: u64,
    pub device_revision: u64,
    pub connected: bool,
    pub connection_label: String,
    pub computer_name: String,
    pub local_ble_host: Option<u8>,
    pub local_wired: bool,
    pub notifications_enabled: bool,
    pub notification_prompt_seen: bool,
    pub autostart_enabled: bool,
}

pub enum BrowserTransport {
    Tauri(TauriTransport),
    Bluetooth(BluetoothTransport),
    Hid(HidTransport),
}

impl BrowserTransport {
    pub async fn connect_bluetooth(
        on_bytes: BytesCallback,
        on_disconnect: DisconnectCallback,
        on_event: BytesCallback,
    ) -> Result<Rc<Self>, String> {
        if tauri_invoke().is_some() {
            return Ok(Rc::new(Self::Tauri(
                TauriTransport::connect(on_bytes).await?,
            )));
        }
        Ok(Rc::new(Self::Bluetooth(
            BluetoothTransport::connect(on_bytes, on_disconnect, on_event).await?,
        )))
    }

    pub async fn connect_hid(
        on_bytes: BytesCallback,
        on_disconnect: DisconnectCallback,
        on_event: BytesCallback,
    ) -> Result<Rc<Self>, String> {
        if tauri_invoke().is_some() {
            return Ok(Rc::new(Self::Tauri(
                TauriTransport::connect(on_bytes).await?,
            )));
        }
        Ok(Rc::new(Self::Hid(
            HidTransport::connect(on_bytes, on_disconnect, on_event).await?,
        )))
    }

    pub async fn write(&self, request: PendingRequest) -> Result<(), String> {
        match self {
            Self::Tauri(transport) => transport.write(request).await,
            Self::Bluetooth(transport) => transport.write(request).await,
            Self::Hid(transport) => transport.write(request).await,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Tauri(transport) => transport.label.clone(),
            Self::Bluetooth(_) => "Bluetooth · HIDShift".into(),
            Self::Hid(_) => "有線 · HID".into(),
        }
    }
}

pub struct TauriTransport {
    on_bytes: BytesCallback,
    label: String,
}

impl TauriTransport {
    async fn connect(on_bytes: BytesCallback) -> Result<Self, String> {
        let value = tauri_call("connect_hidshift", &Object::new()).await?;
        let label = value
            .as_string()
            .unwrap_or_else(|| "Companion · HIDShift".into());
        Ok(Self { on_bytes, label })
    }

    async fn write(&self, request: PendingRequest) -> Result<(), String> {
        let args = Object::new();
        let request_bytes = Array::new();
        for byte in request.encode() {
            request_bytes.push(&JsValue::from_f64(f64::from(byte)));
        }
        Reflect::set(&args, &"request".into(), &request_bytes).map_err(js_error)?;
        let value = tauri_call("management_request", &args).await?;
        let bytes = Uint8Array::new(&value).to_vec();
        (self.on_bytes)(&bytes);
        Ok(())
    }
}

fn tauri_invoke() -> Option<Function> {
    let window: JsValue = web_sys::window()?.into();
    let internals = Reflect::get(&window, &"__TAURI_INTERNALS__".into()).ok()?;
    Reflect::get(&internals, &"invoke".into())
        .ok()?
        .dyn_into()
        .ok()
}

pub(crate) fn is_tauri() -> bool {
    tauri_invoke().is_some()
}

pub(crate) async fn native_snapshot() -> Result<NativeCompanionSnapshot, String> {
    let value = tauri_call("companion_snapshot", &Object::new()).await?;
    parse_native_snapshot(&value)
}

pub(crate) async fn set_native_notifications(
    enabled: bool,
) -> Result<NativeCompanionSnapshot, String> {
    let args = Object::new();
    Reflect::set(&args, &"enabled".into(), &JsValue::from_bool(enabled)).map_err(js_error)?;
    let value = tauri_call("set_notifications_enabled", &args).await?;
    parse_native_snapshot(&value)
}

pub(crate) async fn set_native_autostart(enabled: bool) -> Result<(), String> {
    let args = Object::new();
    Reflect::set(&args, &"enabled".into(), &JsValue::from_bool(enabled)).map_err(js_error)?;
    tauri_call("set_autostart", &args).await.map(|_| ())
}

pub(crate) async fn listen_native_snapshots(
    callback: Rc<dyn Fn(NativeCompanionSnapshot)>,
) -> Result<(), String> {
    let window: JsValue = web_sys::window().ok_or("window is unavailable")?.into();
    let internals = Reflect::get(&window, &"__TAURI_INTERNALS__".into()).map_err(js_error)?;
    let transform: Function = Reflect::get(&internals, &"transformCallback".into())
        .map_err(js_error)?
        .dyn_into()
        .map_err(|_| "Tauri transformCallback is unavailable")?;
    let closure = Closure::wrap(Box::new(move |event: JsValue| {
        let payload = Reflect::get(&event, &"payload".into()).unwrap_or(JsValue::UNDEFINED);
        if let Ok(snapshot) = parse_native_snapshot(&payload) {
            callback(snapshot);
        }
    }) as Box<dyn FnMut(JsValue)>);
    let handler = transform
        .call2(
            &JsValue::UNDEFINED,
            closure.as_ref().unchecked_ref(),
            &JsValue::FALSE,
        )
        .map_err(js_error)?;
    let args = Object::new();
    Reflect::set(&args, &"event".into(), &"hidshift://companion-state".into()).map_err(js_error)?;
    let target = Object::new();
    Reflect::set(&target, &"kind".into(), &"Any".into()).map_err(js_error)?;
    Reflect::set(&args, &"target".into(), &target).map_err(js_error)?;
    Reflect::set(&args, &"handler".into(), &handler).map_err(js_error)?;
    tauri_call("plugin:event|listen", &args).await?;
    // App owns the listener for the full WebView lifetime.
    closure.forget();
    Ok(())
}

fn parse_native_snapshot(value: &JsValue) -> Result<NativeCompanionSnapshot, String> {
    let string = |name: &str| {
        Reflect::get(value, &name.into())
            .map_err(js_error)?
            .as_string()
            .ok_or_else(|| format!("missing native snapshot field {name}"))
    };
    let boolean = |name: &str| {
        Reflect::get(value, &name.into())
            .map_err(js_error)?
            .as_bool()
            .ok_or_else(|| format!("missing native snapshot field {name}"))
    };
    let number = |name: &str| {
        Reflect::get(value, &name.into())
            .map_err(js_error)?
            .as_f64()
            .map(|number| number as u64)
            .ok_or_else(|| format!("missing native snapshot field {name}"))
    };
    let kind = string("connection_kind")?;
    let local_ble = Reflect::get(value, &"local_ble_host".into()).map_err(js_error)?;
    Ok(NativeCompanionSnapshot {
        revision: number("revision")?,
        device_revision: number("device_revision")?,
        connected: kind != "searching",
        connection_label: string("connection_label")?,
        computer_name: string("computer_name")?,
        local_ble_host: local_ble.as_f64().map(|value| value as u8),
        local_wired: boolean("local_wired")?,
        notifications_enabled: boolean("notifications_enabled")?,
        notification_prompt_seen: boolean("notification_prompt_seen")?,
        autostart_enabled: boolean("autostart_enabled")?,
    })
}

async fn tauri_call(command: &str, args: &Object) -> Result<JsValue, String> {
    let invoke = tauri_invoke().ok_or("Tauri IPC is unavailable")?;
    let promise: Promise = invoke
        .call2(&JsValue::UNDEFINED, &command.into(), args)
        .map_err(js_error)?
        .dyn_into()
        .map_err(|_| "Tauri invoke did not return a Promise")?;
    JsFuture::from(promise).await.map_err(js_error)
}

pub struct BluetoothTransport {
    request_characteristic: JsValue,
    // Keep the notification source alive for as long as the transport. The JS
    // event listener alone does not guarantee that the characteristic wrapper
    // remains rooted after `connect` returns.
    response_characteristic: JsValue,
    on_bytes: BytesCallback,
    _notification: Closure<dyn FnMut(Event)>,
    _event_characteristic: JsValue,
    _event_notification: Closure<dyn FnMut(Event)>,
    _disconnect: Closure<dyn FnMut(Event)>,
}

impl BluetoothTransport {
    async fn connect(
        on_bytes: BytesCallback,
        on_disconnect: DisconnectCallback,
        on_event: BytesCallback,
    ) -> Result<Self, String> {
        let navigator = web_sys::window()
            .ok_or("window is unavailable")?
            .navigator();
        let bluetooth =
            Reflect::get(&navigator, &JsValue::from_str("bluetooth")).map_err(js_error)?;
        if bluetooth.is_undefined() {
            let window: JsValue = web_sys::window().ok_or("window is unavailable")?.into();
            let secure = Reflect::get(&window, &"isSecureContext".into())
                .ok()
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            return Err(if secure {
                "このブラウザでWeb Bluetooth APIを利用できません".into()
            } else {
                "Web BluetoothにはHTTPSまたはlocalhostが必要です。http://localhostで開いてください"
                    .into()
            });
        }

        let filter = Object::new();
        let services = Array::new();
        services.push(&JsValue::from_str(MANAGEMENT_SERVICE_UUID));
        Reflect::set(&filter, &"services".into(), &services).map_err(js_error)?;
        let filters = Array::new();
        filters.push(&filter);
        let options = Object::new();
        Reflect::set(&options, &"filters".into(), &filters).map_err(js_error)?;

        let device = await_method(&bluetooth, "requestDevice", &[options.into()]).await?;
        let gatt = Reflect::get(&device, &"gatt".into()).map_err(js_error)?;
        let server = await_method(&gatt, "connect", &[]).await?;
        let service = await_method(
            &server,
            "getPrimaryService",
            &[MANAGEMENT_SERVICE_UUID.into()],
        )
        .await?;
        let request_characteristic = await_method(
            &service,
            "getCharacteristic",
            &[MANAGEMENT_REQUEST_UUID.into()],
        )
        .await?;
        let response_characteristic = await_method(
            &service,
            "getCharacteristic",
            &[MANAGEMENT_RESPONSE_UUID.into()],
        )
        .await?;
        await_method(&response_characteristic, "startNotifications", &[]).await?;
        let event_characteristic = await_method(
            &service,
            "getCharacteristic",
            &[MANAGEMENT_EVENT_UUID.into()],
        )
        .await?;
        await_method(&event_characteristic, "startNotifications", &[]).await?;

        let response_target: EventTarget = response_characteristic
            .clone()
            .dyn_into()
            .map_err(|_| "Bluetooth response characteristic is not an EventTarget")?;
        let notification_on_bytes = on_bytes.clone();
        let notification = Closure::wrap(Box::new(move |event: Event| {
            // Forward an empty frame on extraction failure so the pending request
            // reports the transport/protocol problem instead of becoming a timeout.
            let bytes = bluetooth_event_bytes(&event).unwrap_or_default();
            notification_on_bytes(&bytes);
        }) as Box<dyn FnMut(Event)>);
        response_target
            .add_event_listener_with_callback(
                "characteristicvaluechanged",
                notification.as_ref().unchecked_ref(),
            )
            .map_err(js_error)?;

        let event_target: EventTarget = event_characteristic
            .clone()
            .dyn_into()
            .map_err(|_| "Bluetooth event characteristic is not an EventTarget")?;
        let event_notification = Closure::wrap(Box::new(move |event: Event| {
            let bytes = bluetooth_event_bytes(&event).unwrap_or_default();
            if bytes.len() == MANAGEMENT_EVENT_LEN {
                on_event(&bytes);
            }
        }) as Box<dyn FnMut(Event)>);
        event_target
            .add_event_listener_with_callback(
                "characteristicvaluechanged",
                event_notification.as_ref().unchecked_ref(),
            )
            .map_err(js_error)?;

        let device_target: EventTarget = device
            .dyn_into()
            .map_err(|_| "Bluetooth device is not an EventTarget")?;
        let disconnect = Closure::wrap(Box::new(move |_event: Event| {
            on_disconnect("Bluetooth 接続が切れました".into());
        }) as Box<dyn FnMut(Event)>);
        device_target
            .add_event_listener_with_callback(
                "gattserverdisconnected",
                disconnect.as_ref().unchecked_ref(),
            )
            .map_err(js_error)?;

        Ok(Self {
            request_characteristic,
            response_characteristic,
            _event_characteristic: event_characteristic,
            on_bytes,
            _notification: notification,
            _event_notification: event_notification,
            _disconnect: disconnect,
        })
    }

    async fn write(&self, request: PendingRequest) -> Result<(), String> {
        let request_id = request.request().request_id;
        let bytes = Uint8Array::from(request.encode().as_slice());
        await_method(
            &self.request_characteristic,
            "writeValueWithResponse",
            &[bytes.into()],
        )
        .await?;
        for _ in 0..50 {
            if let Ok(value) = await_method(&self.response_characteristic, "readValue", &[]).await
                && let Some(response) = bluetooth_value_bytes(&value)
                && response_matches_request(&response, request_id)
            {
                (self.on_bytes)(&response);
                break;
            }
            TimeoutFuture::new(20).await;
        }
        Ok(())
    }
}

pub struct HidTransport {
    device: JsValue,
    _input_report: Closure<dyn FnMut(Event)>,
    _disconnect: Closure<dyn FnMut(Event)>,
}

impl HidTransport {
    async fn connect(
        on_bytes: BytesCallback,
        on_disconnect: DisconnectCallback,
        on_event: BytesCallback,
    ) -> Result<Self, String> {
        let navigator = web_sys::window()
            .ok_or("window is unavailable")?
            .navigator();
        let hid = Reflect::get(&navigator, &"hid".into()).map_err(js_error)?;
        if hid.is_undefined() {
            return Err("このブラウザはWebHIDに対応していません".into());
        }
        let filter = Object::new();
        Reflect::set(
            &filter,
            &"vendorId".into(),
            &JsValue::from_f64(f64::from(hidshift::fallback::FALLBACK_USB_VENDOR_ID)),
        )
        .map_err(js_error)?;
        Reflect::set(
            &filter,
            &"productId".into(),
            &JsValue::from_f64(f64::from(hidshift::fallback::FALLBACK_USB_PRODUCT_ID)),
        )
        .map_err(js_error)?;
        Reflect::set(
            &filter,
            &"usagePage".into(),
            &JsValue::from_f64(f64::from(MANAGEMENT_HID_USAGE_PAGE)),
        )
        .map_err(js_error)?;
        Reflect::set(
            &filter,
            &"usage".into(),
            &JsValue::from_f64(f64::from(MANAGEMENT_HID_USAGE)),
        )
        .map_err(js_error)?;
        let filters = Array::new();
        filters.push(&filter);
        let options = Object::new();
        Reflect::set(&options, &"filters".into(), &filters).map_err(js_error)?;
        let devices = Array::from(&await_method(&hid, "requestDevice", &[options.into()]).await?);
        let device = devices.get(0);
        if device.is_undefined() {
            return Err("HIDShiftが選択されませんでした".into());
        }
        await_method(&device, "open", &[]).await?;

        let device_target: EventTarget = device
            .clone()
            .dyn_into()
            .map_err(|_| "HID device is not an EventTarget")?;
        let input_report =
            Closure::wrap(Box::new(move |event: Event| match hid_event_frame(&event) {
                Some(HidManagementFrame::Response(response)) => on_bytes(&response),
                Some(HidManagementFrame::Event(event)) => on_event(&event),
                None => {}
            }) as Box<dyn FnMut(Event)>);
        device_target
            .add_event_listener_with_callback("inputreport", input_report.as_ref().unchecked_ref())
            .map_err(js_error)?;

        let disconnected_device = device.clone();
        let disconnect = Closure::wrap(Box::new(move |event: Event| {
            let event_device = Reflect::get(&event, &"device".into()).unwrap_or_default();
            if Object::is(&event_device, &disconnected_device) {
                on_disconnect("USB HID接続が切れました".into());
            }
        }) as Box<dyn FnMut(Event)>);
        let hid_target: EventTarget = hid
            .dyn_into()
            .map_err(|_| "WebHID manager is not an EventTarget")?;
        hid_target
            .add_event_listener_with_callback("disconnect", disconnect.as_ref().unchecked_ref())
            .map_err(js_error)?;

        Ok(Self {
            device,
            _input_report: input_report,
            _disconnect: disconnect,
        })
    }

    async fn write(&self, request: PendingRequest) -> Result<(), String> {
        let packet = encode_hid_request(request);
        let bytes = Uint8Array::from(&packet[1..]);
        await_method(
            &self.device,
            "sendReport",
            &[
                JsValue::from_f64(f64::from(MANAGEMENT_HID_REQUEST_REPORT_ID)),
                bytes.into(),
            ],
        )
        .await?;
        Ok(())
    }
}

fn hid_event_frame(event: &Event) -> Option<HidManagementFrame> {
    let report_id = Reflect::get(event, &"reportId".into()).ok()?.as_f64()? as u8;
    let value = Reflect::get(event, &"data".into()).ok()?;
    let bytes = data_view_bytes(&value)?;
    decode_hid_input(report_id, &bytes)
}

fn bluetooth_event_bytes(event: &Event) -> Option<Vec<u8>> {
    let target = event.target().or_else(|| event.current_target())?;
    let value = Reflect::get(&target, &"value".into()).ok()?;
    bluetooth_value_bytes(&value)
}

fn bluetooth_value_bytes(value: &JsValue) -> Option<Vec<u8>> {
    let bytes = data_view_bytes(value)?;
    (bytes.len() == MANAGEMENT_RESPONSE_LEN).then_some(bytes)
}

fn data_view_bytes(value: &JsValue) -> Option<Vec<u8>> {
    let buffer = Reflect::get(value, &"buffer".into()).ok()?;
    let offset = Reflect::get(value, &"byteOffset".into()).ok()?.as_f64()? as u32;
    let length = Reflect::get(value, &"byteLength".into()).ok()?.as_f64()? as u32;
    Some(Uint8Array::new_with_byte_offset_and_length(&buffer, offset, length).to_vec())
}

fn response_matches_request(response: &[u8], request_id: u8) -> bool {
    response.len() == MANAGEMENT_RESPONSE_LEN && response.get(1) == Some(&request_id)
}

async fn await_method(target: &JsValue, name: &str, args: &[JsValue]) -> Result<JsValue, String> {
    let value = call_method(target, name, args)?;
    let promise: Promise = value
        .dyn_into()
        .map_err(|_| format!("{name} did not return a Promise"))?;
    JsFuture::from(promise).await.map_err(js_error)
}

fn call_method(target: &JsValue, name: &str, args: &[JsValue]) -> Result<JsValue, String> {
    let function: Function = Reflect::get(target, &JsValue::from_str(name))
        .map_err(js_error)?
        .dyn_into()
        .map_err(|_| format!("browser method {name} is unavailable"))?;
    let arguments = Array::new();
    for argument in args {
        arguments.push(argument);
    }
    function.apply(target, &arguments).map_err(js_error)
}

fn js_error(error: JsValue) -> String {
    error
        .as_string()
        .or_else(|| Reflect::get(&error, &"message".into()).ok()?.as_string())
        .unwrap_or_else(|| format!("{error:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_fallback_accepts_only_the_current_response() {
        let mut response = [0; MANAGEMENT_RESPONSE_LEN];
        response[1] = 7;

        assert!(response_matches_request(&response, 7));
        assert!(!response_matches_request(&response, 6));
        assert!(!response_matches_request(
            &response[..MANAGEMENT_RESPONSE_LEN - 1],
            7
        ));
    }
}
