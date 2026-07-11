use std::cell::RefCell;
use std::rc::Rc;

use hidshift::{
    MANAGEMENT_REQUEST_UUID, MANAGEMENT_RESPONSE_LEN, MANAGEMENT_RESPONSE_UUID,
    MANAGEMENT_SERVICE_UUID,
};
use hidshift_client::{PendingRequest, SerialResponseDecoder, encode_serial_request};
use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{Event, EventTarget};

type BytesCallback = Rc<dyn Fn(&[u8])>;
type DisconnectCallback = Rc<dyn Fn(String)>;

pub enum BrowserTransport {
    Bluetooth(BluetoothTransport),
    Serial(SerialTransport),
}

impl BrowserTransport {
    pub async fn connect_bluetooth(
        on_bytes: BytesCallback,
        on_disconnect: DisconnectCallback,
    ) -> Result<Rc<Self>, String> {
        Ok(Rc::new(Self::Bluetooth(
            BluetoothTransport::connect(on_bytes, on_disconnect).await?,
        )))
    }

    pub async fn connect_serial(
        on_bytes: BytesCallback,
        on_disconnect: DisconnectCallback,
    ) -> Result<Rc<Self>, String> {
        Ok(Rc::new(Self::Serial(
            SerialTransport::connect(on_bytes, on_disconnect).await?,
        )))
    }

    pub async fn write(&self, request: PendingRequest) -> Result<(), String> {
        match self {
            Self::Bluetooth(transport) => transport.write(request).await,
            Self::Serial(transport) => transport.write(request).await,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Bluetooth(_) => "Bluetooth · HIDShift".into(),
            Self::Serial(_) => "有線 · Serial".into(),
        }
    }
}

pub struct BluetoothTransport {
    request_characteristic: JsValue,
    _notification: Closure<dyn FnMut(Event)>,
    _disconnect: Closure<dyn FnMut(Event)>,
}

impl BluetoothTransport {
    async fn connect(
        on_bytes: BytesCallback,
        on_disconnect: DisconnectCallback,
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

        let response_target: EventTarget = response_characteristic
            .clone()
            .dyn_into()
            .map_err(|_| "Bluetooth response characteristic is not an EventTarget")?;
        let notification = Closure::wrap(Box::new(move |event: Event| {
            if let Some(bytes) = bluetooth_event_bytes(&event) {
                on_bytes(&bytes);
            }
        }) as Box<dyn FnMut(Event)>);
        response_target
            .add_event_listener_with_callback(
                "characteristicvaluechanged",
                notification.as_ref().unchecked_ref(),
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
            _notification: notification,
            _disconnect: disconnect,
        })
    }

    async fn write(&self, request: PendingRequest) -> Result<(), String> {
        let bytes = Uint8Array::from(request.encode().as_slice());
        await_method(
            &self.request_characteristic,
            "writeValueWithResponse",
            &[bytes.into()],
        )
        .await?;
        Ok(())
    }
}

pub struct SerialTransport {
    writer: JsValue,
}

impl SerialTransport {
    async fn connect(
        on_bytes: BytesCallback,
        on_disconnect: DisconnectCallback,
    ) -> Result<Self, String> {
        let navigator = web_sys::window()
            .ok_or("window is unavailable")?
            .navigator();
        let serial = Reflect::get(&navigator, &"serial".into()).map_err(js_error)?;
        if serial.is_undefined() {
            return Err("このブラウザは Web Serial に対応していません".into());
        }
        let request_options = Object::new();
        let port = await_method(&serial, "requestPort", &[request_options.into()]).await?;
        let options = Object::new();
        Reflect::set(&options, &"baudRate".into(), &JsValue::from_f64(115_200.0))
            .map_err(js_error)?;
        await_method(&port, "open", &[options.into()]).await?;
        let writable = Reflect::get(&port, &"writable".into()).map_err(js_error)?;
        let writer = call_method(&writable, "getWriter", &[])?;
        let readable = Reflect::get(&port, &"readable".into()).map_err(js_error)?;
        let reader = call_method(&readable, "getReader", &[])?;

        spawn_local(read_serial(reader, on_bytes, on_disconnect));
        Ok(Self { writer })
    }

    async fn write(&self, request: PendingRequest) -> Result<(), String> {
        let line = encode_serial_request(request);
        let bytes = Uint8Array::from(line.as_slice());
        await_method(&self.writer, "write", &[bytes.into()]).await?;
        Ok(())
    }
}

async fn read_serial(reader: JsValue, on_bytes: BytesCallback, on_disconnect: DisconnectCallback) {
    let decoder = Rc::new(RefCell::new(SerialResponseDecoder::default()));
    loop {
        let result = match await_method(&reader, "read", &[]).await {
            Ok(result) => result,
            Err(error) => {
                on_disconnect(error);
                return;
            }
        };
        let done = Reflect::get(&result, &"done".into())
            .ok()
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if done {
            on_disconnect("有線接続が切れました".into());
            return;
        }
        let Ok(value) = Reflect::get(&result, &"value".into()) else {
            continue;
        };
        let bytes = Uint8Array::new(&value).to_vec();
        for response in decoder.borrow_mut().push(&bytes) {
            on_bytes(&response);
        }
    }
}

fn bluetooth_event_bytes(event: &Event) -> Option<Vec<u8>> {
    let target = event.target()?;
    let value = Reflect::get(&target, &"value".into()).ok()?;
    let buffer = Reflect::get(&value, &"buffer".into()).ok()?;
    let offset = Reflect::get(&value, &"byteOffset".into()).ok()?.as_f64()? as u32;
    let length = Reflect::get(&value, &"byteLength".into()).ok()?.as_f64()? as u32;
    if length as usize != MANAGEMENT_RESPONSE_LEN {
        return None;
    }
    Some(Uint8Array::new_with_byte_offset_and_length(&buffer, offset, length).to_vec())
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
