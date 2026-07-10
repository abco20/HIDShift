use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Sender;
use embassy_time::{Duration, Timer};
use esp_hal::gpio::{Input, InputConfig, Pull};
use hidshift::runtime::RUNTIME_INPUT_QUEUE_CAPACITY;
use hidshift::runtime::message::RuntimeInputMessage;
use hidshift::target_control::TargetSwitchControl;

pub const TARGET_BUTTON_SAMPLE_MS: u64 = 5;
pub const TARGET_BUTTON_TICK_MS: u64 = 250;

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

    let mut now_ms = 0u64;
    let mut tick_accum_ms = 0u64;
    let mut last_pressed = button.is_low();
    log::debug!(
        "firmware: target button GPIO0 initial_pressed={}",
        last_pressed
    );

    loop {
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
        now_ms = now_ms.saturating_add(TARGET_BUTTON_SAMPLE_MS);
        tick_accum_ms = tick_accum_ms.saturating_add(TARGET_BUTTON_SAMPLE_MS);
        if tick_accum_ms >= TARGET_BUTTON_TICK_MS {
            tick_accum_ms = 0;
            sender.send(RuntimeInputMessage::Tick { now_ms }).await;
        }
    }
}
