use astrobox_ng_wit::astrobox::psys_host_v4::ui as host_ui;
use astrobox_ng_wit::exports::astrobox::psys_plugin_v4::{event, lifecycle};

pub mod crpack;
pub mod firmware;
pub mod logger;
pub mod lvgl;
pub mod ui;

struct MyPlugin;

impl event::Guest for MyPlugin {
    async fn on_event(event_type: event::EventType, event_payload: String) -> String {
        match event_type {
            event::EventType::PluginMessage => {}
            event::EventType::InterconnectMessage => {}
            event::EventType::DeviceAction => {}
            event::EventType::ProviderAction => {}
            event::EventType::DeeplinkAction => {}
            event::EventType::TransportPacket => {}
            event::EventType::Timer => {}
        }

        tracing::info!("event_payload: {}", event_payload);
        String::new()
    }

    async fn on_ui_event(event_id: String, event: host_ui::Event, event_payload: String) -> String {
        ui::ui_event_processor(event, &event_id, &event_payload).await;
        String::new()
    }

    async fn on_ui_render(element_id: String) {
        ui::render_main_ui(&element_id);
    }

    async fn on_card_render(_card_id: String) {}
}

impl lifecycle::Guest for MyPlugin {
    async fn on_load() {
        logger::init();
        tracing::info!("Hello AstroBox API Level 4 Plugin!");
    }
}

astrobox_ng_wit::export!(MyPlugin);
