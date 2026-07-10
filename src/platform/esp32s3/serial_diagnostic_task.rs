use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Sender;
use embassy_time::Timer;
use esp_hal::peripherals::{GPIO44, UART0};
use esp_hal::uart::{Config, Uart};
use hidshift::bridge::BridgeEvent;
use hidshift::ids::{DeviceId, HostId};
use hidshift::input::{
    InputFrame, KeyUsage, KeyboardFrame, ModifierState, MouseButtons, MouseFrame, MouseMovement,
    StandardInputFrame,
};
use hidshift::runtime::RUNTIME_INPUT_QUEUE_CAPACITY;
use hidshift::runtime::message::RuntimeInputMessage;

const DIAGNOSTIC_DEVICE_ID: DeviceId = DeviceId(0xf0);

#[embassy_executor::task]
pub async fn serial_diagnostic_task(
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    uart: UART0<'static>,
    rx: GPIO44<'static>,
) {
    let Ok(uart) = Uart::new(uart, Config::default()) else {
        esp_println::println!("firmware: diagnostic UART init failed");
        return;
    };
    let mut uart = uart.with_rx(rx).into_async();

    esp_println::println!("firmware: diagnostic UART ready command=a");
    let mut byte = [0u8; 1];
    loop {
        match uart.read_async(&mut byte).await {
            Ok(1) if byte[0] == b'a' => run_diagnostic_sequence(sender).await,
            Ok(1) if byte[0] == b'1' => switch_target(sender, HostId(1)).await,
            Ok(1) if byte[0] == b'2' => switch_target(sender, HostId(2)).await,
            Ok(1) if byte[0] == b'x' => clear_host(sender, HostId(1)).await,
            Ok(1) if byte[0] == b'c' => clear_host(sender, HostId(2)).await,
            Ok(1) if byte[0] == b'p' => pair_host_2(sender).await,
            Ok(1) if byte[0] == b'?' => {
                esp_println::println!(
                    "firmware: diagnostic commands a=input 1/2=target x=clear1 c=clear2 p=pair2"
                );
            }
            Ok(_) => {}
            Err(error) => {
                esp_println::println!("firmware: diagnostic UART read failed: {:?}", error);
            }
        }
    }
}

async fn clear_host(
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    host_id: HostId,
) {
    esp_println::println!("firmware: diagnostic reset bond host={}", host_id.0);
    sender
        .send(RuntimeInputMessage::BridgeEvent(BridgeEvent::ClearHost {
            host_id,
        }))
        .await;
}

async fn pair_host_2(
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
) {
    esp_println::println!("firmware: diagnostic pairing host=2");
    sender
        .send(RuntimeInputMessage::BridgeEvent(
            BridgeEvent::EnterPairingMode { host_id: HostId(2) },
        ))
        .await;
}

async fn switch_target(
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    target: HostId,
) {
    esp_println::println!("firmware: diagnostic switch target={}", target.0);
    sender
        .send(RuntimeInputMessage::BridgeEvent(
            BridgeEvent::SwitchTarget { target },
        ))
        .await;
}

async fn run_diagnostic_sequence(
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
) {
    esp_println::println!("firmware: diagnostic sequence start");

    send_frame(
        sender,
        Some(KeyboardFrame::new(ModifierState::empty())),
        Some(neutral_mouse()),
    )
    .await;
    Timer::after_millis(150).await;

    let mut keyboard = KeyboardFrame::new(ModifierState::empty());
    if keyboard.push_key(KeyUsage(0x04)).is_err() {
        esp_println::println!("firmware: diagnostic key frame build failed");
        return;
    }
    esp_println::println!("firmware: diagnostic keyboard A press");
    send_frame(sender, Some(keyboard), None).await;
    Timer::after_millis(100).await;

    esp_println::println!("firmware: diagnostic keyboard release");
    send_frame(
        sender,
        Some(KeyboardFrame::new(ModifierState::empty())),
        None,
    )
    .await;
    Timer::after_millis(150).await;

    esp_println::println!("firmware: diagnostic mouse move x=20 y=-12");
    send_frame(
        sender,
        None,
        Some(MouseFrame {
            buttons: MouseButtons::empty(),
            movement: MouseMovement {
                x: 20,
                y: -12,
                wheel: 0,
                pan: 0,
            },
        }),
    )
    .await;
    Timer::after_millis(150).await;

    esp_println::println!("firmware: diagnostic mouse left press");
    send_frame(
        sender,
        None,
        Some(MouseFrame {
            buttons: MouseButtons::LEFT,
            movement: MouseMovement::neutral(),
        }),
    )
    .await;
    Timer::after_millis(100).await;

    esp_println::println!("firmware: diagnostic mouse release");
    send_frame(sender, None, Some(neutral_mouse())).await;
    esp_println::println!("firmware: diagnostic sequence complete");
}

const fn neutral_mouse() -> MouseFrame {
    MouseFrame {
        buttons: MouseButtons::empty(),
        movement: MouseMovement::neutral(),
    }
}

async fn send_frame(
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    keyboard: Option<KeyboardFrame>,
    mouse: Option<MouseFrame>,
) {
    sender
        .send(RuntimeInputMessage::BridgeEvent(BridgeEvent::InputFrame(
            InputFrame::Standard(StandardInputFrame {
                device_id: DIAGNOSTIC_DEVICE_ID,
                keyboard,
                mouse,
                consumer: None,
            }),
        )))
        .await;
}
