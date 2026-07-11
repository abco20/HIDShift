use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Sender;
use embassy_time::{Duration, Instant, Timer};
use esp_hal::gpio::{Input, InputConfig, Pull};
use hidshift::runtime::RUNTIME_INPUT_QUEUE_CAPACITY;
use hidshift::runtime::message::RuntimeInputMessage;
use hidshift::target_control::TargetSwitchControl;

pub const TARGET_BUTTON_SAMPLE_MS: u64 = 5;
// Runtime deadlines include a configurable target-switch delay as short as 5 ms.
// A 10 ms tick keeps that setting responsive without flooding the input queue.
pub const TARGET_BUTTON_TICK_MS: u64 = 10;

#[embassy_executor::task]
pub async fn control_task(
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    button_pin: esp_hal::peripherals::GPIO0<'static>,
) {
    log::info!("firmware: control task boot");

    let button_config = InputConfig::default().with_pull(Pull::Up);
    let button = Input::new(button_pin, button_config);
    let mut target_control = TargetSwitchControl::new();

    let mut next_tick = Instant::now() + Duration::from_millis(TARGET_BUTTON_TICK_MS);
    let mut last_pressed = button.is_low();
    log::debug!(
        "firmware: target button GPIO0 initial_pressed={}",
        last_pressed
    );

    loop {
        let now = Instant::now();
        let now_ms = now.as_millis();
        let pressed = button.is_low();
        if pressed != last_pressed {
            log::debug!(
                "firmware: target button raw pressed={} at_ms={}",
                pressed,
                now_ms
            );
            last_pressed = pressed;
        }

        if let Some(intent) = target_control.target_button_sample(pressed, now_ms) {
            let message = RuntimeInputMessage::ButtonIntent { intent, now_ms };
            log::info!("firmware: target button runtime_input {:?}", message);
            sender.send(message).await;
        }

        Timer::after(Duration::from_millis(TARGET_BUTTON_SAMPLE_MS)).await;
        let now = Instant::now();
        if now >= next_tick {
            next_tick = now + Duration::from_millis(TARGET_BUTTON_TICK_MS);
            // Runtime ownership is deliberately paused while BLE is quiesced for USB
            // enumeration or flash writes. Coalesce periodic deadline hints to at most
            // one queued message so they cannot consume capacity needed by the operation
            // which will resume the runtime. Dropping ticks is safe: deadlines are
            // absolute and the next accepted tick evaluates them again.
            if sender.free_capacity() == RUNTIME_INPUT_QUEUE_CAPACITY {
                let _ = sender.try_send(RuntimeInputMessage::Tick {
                    now_ms: now.as_millis(),
                });
            }
        }
    }
}
