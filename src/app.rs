// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use bluer::{AdapterProperty, Session};
use cosmic::{
    Element, Task,
    applet::{
        padded_control,
        token::subscription::{TokenRequest, TokenUpdate, activation_token_subscription},
    },
    cctk::sctk::reexports::calloop,
    cosmic_theme::Spacing,
    iced::{Alignment, Length, Limits, Subscription, futures::StreamExt, window::Id},
    widget::{button, column, container, divider, icon, row, slider, text},
};
use cosmic_settings_audio_client::{self as audio_client, CosmicAudioProxy};
use cosmic_settings_daemon_subscription as settings_daemon;
use cosmic_settings_upower_subscription::device::{DeviceDbusEvent, device_subscription};
use futures::SinkExt;
use logind_zbus::manager::ManagerProxy;
use nmrs::{ActiveConnection, NetworkManager, NetworkSnapshot};
use tokio::sync::mpsc::UnboundedSender;
use zbus::Connection;

use crate::fl;

/// The model intentionally keeps only the small set of values exposed by this MVP.  Each
/// service is optional: a missing daemon simply removes (or disables) its control.
#[derive(Default)]
pub struct AppModel {
    core: cosmic::Core,
    popup: Option<Id>,
    token_tx: Option<calloop::channel::Sender<TokenRequest>>,
    settings_connection: Option<Connection>,
    brightness_sender: Option<UnboundedSender<settings_daemon::Request>>,
    network: NetworkState,
    bluetooth: Option<bool>,
    audio: AudioState,
    brightness: Option<BrightnessState>,
    brightness_value: Option<i32>,
    brightness_max: Option<i32>,
    battery: Option<BatteryState>,
    profile: Option<PowerProfile>,
}

#[derive(Default)]
struct NetworkState {
    wifi_enabled: Option<bool>,
    connectivity: Option<String>,
    connection_name: Option<String>,
}

#[derive(Default)]
struct AudioState {
    client: Option<Arc<audio_client::Client>>,
    default_sink: Option<u32>,
    volume: u32,
    mute: bool,
    nodes: HashMap<u32, (u32, bool)>,
    pending_volume: Arc<AtomicU32>,
}

impl AudioState {
    #[allow(clippy::needless_pass_by_value)]
    fn apply(&mut self, event: audio_client::Event) {
        match event {
            audio_client::Event::NodeVolume(id, volume, _) => {
                let entry = self.nodes.entry(id).or_default();
                entry.0 = volume;
                if self.default_sink == Some(id) {
                    self.volume = volume.min(100);
                }
            }
            audio_client::Event::NodeMute(id, mute) => {
                let entry = self.nodes.entry(id).or_default();
                entry.1 = mute;
                if self.default_sink == Some(id) {
                    self.mute = mute;
                }
            }
            audio_client::Event::DefaultSink(id) => {
                self.default_sink = Some(id);
                if let Some((volume, mute)) = self.nodes.get(&id) {
                    self.volume = (*volume).min(100);
                    self.mute = *mute;
                }
            }
            _ => {}
        }
    }

    fn available(&self) -> bool {
        self.client.is_some() && self.default_sink.is_some()
    }
}

#[derive(Clone, Copy)]
struct BrightnessState {
    value: i32,
    max: i32,
}

#[derive(Clone, Copy)]
struct BatteryState {
    percent: f64,
    on_battery: bool,
    time_to_empty: i64,
}

#[derive(Clone, Debug)]
pub struct PowerProfile {
    backend: ProfileBackend,
    active: String,
    choices: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
enum ProfileBackend {
    System76,
    Upower,
}

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    PopupClosed(Id),
    Network(NetworkUpdate),
    ToggleWifi(bool),
    Bluetooth(BluetoothUpdate),
    ToggleBluetooth(bool),
    AudioClient(Arc<audio_client::Client>),
    AudioClientReturned(Arc<audio_client::Client>),
    Audio(audio_client::Event),
    SetVolume(u32),
    ToggleMute,
    BrightnessDaemon(settings_daemon::Event),
    SetBrightness(i32),
    Battery(DeviceDbusEvent),
    Profile(Result<PowerProfile, String>),
    SetProfile(String),
    SetProfileIndex(usize),
    Surface(cosmic::surface::Action),
    SettingsConnection(Result<Connection, zbus::Error>),
    OpenSettings,
    Token(TokenUpdate),
    Power(PowerAction),
}

#[derive(Debug, Clone)]
pub struct NetworkUpdate {
    wifi_enabled: bool,
    connectivity: String,
    connection_name: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct BluetoothUpdate(bool);

#[derive(Debug, Clone, Copy)]
pub enum PowerAction {
    Suspend,
    Logout,
    Restart,
    Shutdown,
}

impl cosmic::Application for AppModel {
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    type Message = Message;

    // Keep the template's public app ID so existing panel configuration continues to work.
    const APP_ID: &'static str = "com.github.chrispouliot.cosmic-applet-quick-settings";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(
        core: cosmic::Core,
        _flags: Self::Flags,
    ) -> (Self, Task<cosmic::Action<Self::Message>>) {
        (
            Self {
                core,
                ..Self::default()
            },
            Task::perform(Connection::session(), |result| {
                cosmic::Action::App(Message::SettingsConnection(result))
            }),
        )
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn view(&self) -> Element<'_, Message> {
        self.core
            .applet
            .icon_button("display-symbolic")
            .on_press(Message::TogglePopup)
            .into()
    }

    #[allow(clippy::too_many_lines)]
    fn view_window(&self, _id: Id) -> Element<'_, Message> {
        let mut content = column![];

        let network_description = self
            .network
            .connection_name
            .clone()
            .or_else(|| self.network.connectivity.clone())
            .unwrap_or_else(|| fl!("unavailable"));
        let network_caption = if self.network.wifi_enabled == Some(false) {
            format!("{network_description} · {}", fl!("disabled"))
        } else {
            network_description
        };
        let network_tile = quick_control(
            "network-wireless-symbolic",
            fl!("network"),
            network_caption,
            self.network.wifi_enabled == Some(true),
            self.network
                .wifi_enabled
                .map(|enabled| Message::ToggleWifi(!enabled)),
        );
        let bluetooth_tile = quick_control(
            "bluetooth-symbolic",
            fl!("bluetooth"),
            self.bluetooth.map_or_else(
                || fl!("unavailable"),
                |enabled| {
                    if enabled {
                        fl!("enabled")
                    } else {
                        fl!("disabled")
                    }
                },
            ),
            self.bluetooth == Some(true),
            self.bluetooth
                .map(|enabled| Message::ToggleBluetooth(!enabled)),
        );
        content = content.push(
            row![network_tile, bluetooth_tile]
                .width(Length::Fill)
                .spacing(8),
        );

        content = content.push(padded_divider());
        content = content.push(self.audio_row());

        if let Some(brightness) = self.brightness {
            content = content.push(
                row![
                    container(
                        icon::from_name("display-brightness-symbolic")
                            .size(22)
                            .symbolic(true),
                    )
                    .width(Length::Fixed(42.0)),
                    slider(0..=brightness.max, brightness.value, Message::SetBrightness),
                    text(format!("{}%", brightness_percent(brightness))).width(Length::Fixed(42.0)),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            );
        }

        if let Some(battery) = self.battery {
            let caption = battery_caption(battery);
            content = content.push(
                row![
                    icon::from_name(battery_icon_name(battery))
                        .size(24)
                        .symbolic(true),
                    column![text(fl!("battery")), text::caption(caption)].width(Length::Fill),
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            );
        }

        if let Some(profile) = self.profile.as_ref() {
            content = content.push(padded_divider());
            let selected = profile
                .choices
                .iter()
                .position(|choice| choice == &profile.active);
            let labels = profile
                .choices
                .iter()
                .map(|choice| profile_label(choice))
                .collect::<Vec<_>>();
            let dropdown = cosmic::widget::dropdown::popup_dropdown(
                labels,
                selected,
                Message::SetProfileIndex,
                self.popup.unwrap_or(Id::NONE),
                Message::Surface,
                |message| message,
            )
            .width(Length::Fixed(150.0));
            content = content.push(
                row![
                    icon::from_name("preferences-system-symbolic")
                        .size(22)
                        .symbolic(true),
                    text(fl!("power-profile")).width(Length::Fill),
                    dropdown,
                ]
                .spacing(10)
                .align_y(Alignment::Center),
            );
        }

        content = content.push(padded_divider()).push(
            row![
                settings_button(fl!("settings")),
                action_button(
                    "system-suspend-symbolic",
                    fl!("suspend"),
                    PowerAction::Suspend
                ),
                action_button(
                    "system-log-out-symbolic",
                    fl!("logout"),
                    PowerAction::Logout
                ),
                action_button(
                    "system-reboot-symbolic",
                    fl!("restart"),
                    PowerAction::Restart
                ),
                action_button(
                    "system-shutdown-symbolic",
                    fl!("shutdown"),
                    PowerAction::Shutdown
                ),
            ]
            .spacing(2)
            .width(Length::Fill),
        );

        self.core
            .applet
            .popup_container(content.padding([10, 8]))
            .into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        let mut subscriptions = vec![
            network_subscription().map(Message::Network),
            bluetooth_subscription().map(Message::Bluetooth),
            audio_subscription(),
            device_subscription(0).map(Message::Battery),
        ];
        if privileged_wayland_socket_available() {
            subscriptions.push(activation_token_subscription(0).map(Message::Token));
        }
        if let Some(connection) = self.settings_connection.clone() {
            subscriptions
                .push(settings_daemon::subscription(connection).map(Message::BrightnessDaemon));
        }
        Subscription::batch(subscriptions)
    }

    #[allow(clippy::too_many_lines)]
    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        match message {
            Message::TogglePopup => {
                if let Some(popup) = self.popup.take() {
                    return cosmic::surface::surface_task(cosmic::surface::action::destroy_popup(
                        popup,
                    ));
                }
                let id = Id::unique();
                self.popup = Some(id);
                let mut settings = self.core.applet.get_popup_settings(
                    self.core.main_window_id().unwrap(),
                    id,
                    None,
                    None,
                    None,
                );
                settings.positioner.size_limits = Limits::NONE
                    .min_width(320.0)
                    .max_width(400.0)
                    .min_height(300.0)
                    .max_height(900.0);
                return cosmic::iced::platform_specific::shell::wayland::commands::popup::get_popup(
                    settings,
                );
            }
            Message::PopupClosed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                }
            }
            Message::Network(update) => {
                self.network = NetworkState {
                    wifi_enabled: Some(update.wifi_enabled),
                    connectivity: Some(update.connectivity),
                    connection_name: update.connection_name,
                };
            }
            Message::ToggleWifi(enabled) => {
                self.network.wifi_enabled = Some(enabled);
                return Task::perform(set_wifi(enabled), |()| cosmic::Action::None);
            }
            Message::Bluetooth(BluetoothUpdate(enabled)) => self.bluetooth = Some(enabled),
            Message::ToggleBluetooth(enabled) => {
                self.bluetooth = Some(enabled);
                return Task::perform(set_bluetooth(enabled), |()| cosmic::Action::None);
            }
            Message::AudioClient(client) => {
                self.audio.client = Some(client);
                self.audio.nodes.clear();
                self.audio.default_sink = None;
            }
            Message::AudioClientReturned(client) => self.audio.client = Some(client),
            Message::Audio(event) => self.audio.apply(event),
            Message::SetVolume(volume) => {
                self.audio.volume = volume;
                self.audio.pending_volume.store(volume, Ordering::Relaxed);
                if let Some(client) = self.audio.client.take() {
                    let client = match Arc::try_unwrap(client) {
                        Ok(client) => client,
                        Err(client) => {
                            self.audio.client = Some(client);
                            return Task::none();
                        }
                    };
                    let pending_volume = Arc::clone(&self.audio.pending_volume);
                    return Task::perform(set_audio_volume(client, pending_volume), |client| {
                        cosmic::Action::App(Message::AudioClientReturned(client))
                    });
                }
            }
            Message::ToggleMute => {
                if let Some(client) = self.audio.client.take() {
                    let client = match Arc::try_unwrap(client) {
                        Ok(client) => client,
                        Err(client) => {
                            self.audio.client = Some(client);
                            return Task::none();
                        }
                    };
                    return Task::perform(toggle_audio_mute(client), |client| {
                        cosmic::Action::App(Message::AudioClientReturned(client))
                    });
                }
            }
            Message::BrightnessDaemon(event) => match event {
                settings_daemon::Event::Sender(sender) => {
                    self.brightness = None;
                    self.brightness_value = None;
                    self.brightness_max = None;
                    let _ = sender.send(settings_daemon::Request::GetDisplayBrightness);
                    let _ = sender.send(settings_daemon::Request::GetMaxDisplayBrightness);
                    self.brightness_sender = Some(sender);
                }
                settings_daemon::Event::MaxDisplayBrightness(max) if max > 0 => {
                    self.brightness_max = Some(max);
                    self.refresh_brightness();
                }
                settings_daemon::Event::MaxDisplayBrightness(_) => {
                    self.brightness_max = None;
                    self.brightness_value = None;
                    self.brightness = None;
                }
                settings_daemon::Event::DisplayBrightness(value) => {
                    self.brightness_value = (value >= 0).then_some(value);
                    self.refresh_brightness();
                }
            },
            Message::SetBrightness(value) => {
                self.brightness_value = Some(value);
                if let Some(brightness) = &mut self.brightness {
                    brightness.value = value;
                }
                if let Some(sender) = self.brightness_sender.as_ref() {
                    let _ = sender.send(settings_daemon::Request::SetDisplayBrightness(value));
                }
            }
            Message::Battery(event) => match event {
                DeviceDbusEvent::NoBattery => self.battery = None,
                DeviceDbusEvent::Update {
                    on_battery,
                    percent,
                    time_to_empty,
                } => {
                    self.battery = Some(BatteryState {
                        percent,
                        on_battery,
                        time_to_empty,
                    });
                }
            },
            Message::Profile(result) => self.profile = result.ok(),
            Message::SetProfileIndex(index) => {
                if let Some(profile) = self.profile.as_ref()
                    && let Some(name) = profile.choices.get(index).cloned()
                {
                    return self.update(Message::SetProfile(name));
                }
            }
            Message::SetProfile(profile) => {
                if let Some(current) = self.profile.as_mut()
                    && current.choices.iter().any(|choice| choice == &profile)
                {
                    current.active = profile;
                    let current = current.clone();
                    return Task::perform(set_profile(current), |result| {
                        cosmic::Action::App(Message::Profile(result))
                    });
                }
            }
            Message::Surface(action) => {
                return cosmic::task::message(cosmic::Action::Cosmic(
                    cosmic::app::Action::Surface(action),
                ));
            }
            Message::SettingsConnection(result) => {
                if let Ok(connection) = result {
                    self.settings_connection = Some(connection);
                    return Task::perform(get_profile(), |result| {
                        cosmic::Action::App(Message::Profile(result))
                    });
                }
            }
            Message::OpenSettings => {
                if let Some(tx) = self.token_tx.as_ref() {
                    let _ = tx.send(TokenRequest {
                        app_id: Self::APP_ID.to_string(),
                        exec: "cosmic-settings".to_string(),
                    });
                } else if !privileged_wayland_socket_available() {
                    tokio::spawn(cosmic::process::spawn(std::process::Command::new(
                        "cosmic-settings",
                    )));
                }
            }
            Message::Token(update) => match update {
                TokenUpdate::Init(tx) => self.token_tx = Some(tx),
                TokenUpdate::Finished => self.token_tx = None,
                TokenUpdate::ActivationToken { token, .. } => {
                    let mut command = std::process::Command::new("cosmic-settings");
                    if let Some(token) = token {
                        command.env("XDG_ACTIVATION_TOKEN", &token);
                        command.env("DESKTOP_STARTUP_ID", &token);
                    }
                    tokio::spawn(cosmic::process::spawn(command));
                }
            },
            Message::Power(action) => return perform_power_action(action),
        }
        Task::none()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

fn privileged_wayland_socket_available() -> bool {
    std::env::var_os("X_PRIVILEGED_WAYLAND_SOCKET").is_some()
}

fn quick_control(
    icon_name: &'static str,
    title: String,
    caption: String,
    selected: bool,
    on_press: Option<Message>,
) -> Element<'static, Message> {
    let mut control = button::custom(
        column![
            icon::from_name(icon_name).size(24).symbolic(true),
            text(title).width(Length::Fill),
            text::caption(caption).width(Length::Fill),
        ]
        .spacing(2)
        .align_x(Alignment::Center),
    )
    .selected(selected)
    .class(cosmic::theme::Button::ListItem(
        cosmic::theme::active().cosmic().corner_radii.radius_s,
    ))
    .padding([10, 8])
    .width(Length::Fill);
    if let Some(message) = on_press {
        control = control.on_press(message);
    }
    control.into()
}

impl AppModel {
    fn refresh_brightness(&mut self) {
        self.brightness = self
            .brightness_value
            .zip(self.brightness_max)
            .filter(|(_, max)| *max > 0)
            .map(|(value, max)| BrightnessState {
                value: value.clamp(0, max),
                max,
            });
    }

    fn audio_row(&self) -> Element<'_, Message> {
        let icon_name = if self.audio.mute || self.audio.volume == 0 {
            "audio-volume-muted-symbolic"
        } else if self.audio.volume < 50 {
            "audio-volume-medium-symbolic"
        } else {
            "audio-volume-high-symbolic"
        };
        let mut mute = button::icon(icon::from_name(icon_name).size(22).symbolic(true));
        if self.audio.available() {
            mute = mute.on_press(Message::ToggleMute);
        }
        let slider: Element<'_, Message> = if self.audio.available() {
            slider(0..=100, self.audio.volume, Message::SetVolume)
                .width(Length::Fill)
                .into()
        } else {
            text(fl!("unavailable")).into()
        };
        row![
            container(mute).width(Length::Fixed(42.0)),
            slider,
            text(format!("{}%", self.audio.volume)).width(Length::Fixed(42.0))
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .into()
    }
}

fn padded_divider<'a>() -> Element<'a, Message> {
    let Spacing {
        space_xxs, space_s, ..
    } = cosmic::theme::active().cosmic().spacing;
    padded_control(divider::horizontal::default())
        .padding([space_xxs, space_s])
        .into()
}

fn action_button(icon_name: &str, label: String, action: PowerAction) -> Element<'static, Message> {
    button::custom(
        column![
            icon::from_name(icon_name).size(24).symbolic(true),
            text::caption(label)
        ]
        .align_x(Alignment::Center),
    )
    .on_press(Message::Power(action))
    .padding([5, 0])
    .width(Length::Fill)
    .into()
}

fn settings_button(label: String) -> Element<'static, Message> {
    button::custom(
        column![
            icon::from_name("settings-symbolic").size(24).symbolic(true),
            text::caption(label)
        ]
        .align_x(Alignment::Center),
    )
    .on_press(Message::OpenSettings)
    .padding([5, 0])
    .width(Length::Fill)
    .into()
}

fn brightness_percent(state: BrightnessState) -> i32 {
    (state.value.max(0) * 100 / state.max.max(1)).clamp(0, 100)
}

fn battery_icon_name(state: BatteryState) -> String {
    let percent = state.percent.clamp(0.0, 100.0);
    let level = if percent > 95.0 {
        100
    } else if percent > 80.0 {
        90
    } else if percent > 65.0 {
        80
    } else if percent > 35.0 {
        50
    } else if percent > 20.0 {
        35
    } else if percent > 14.0 {
        20
    } else if percent > 9.0 {
        10
    } else if percent > 5.0 {
        5
    } else {
        0
    };
    let charging = if state.on_battery { "" } else { "charging-" };
    format!("cosmic-applet-battery-level-{level}-{charging}symbolic")
}

fn battery_caption(state: BatteryState) -> String {
    let status = if state.on_battery {
        fl!("on-battery")
    } else {
        fl!("charging")
    };
    let percentage = format!("{:.0}%", state.percent.clamp(0.0, 100.0));
    if state.time_to_empty > 0 {
        format!(
            "{percentage} · {status} · {}",
            fl!(
                "battery-time-remaining",
                time = battery_time(state.time_to_empty)
            )
        )
    } else {
        format!("{percentage} · {status}")
    }
}

fn battery_time(seconds: i64) -> String {
    if seconds < 60 {
        return fl!("less-than-minute");
    }
    let minutes = seconds / 60;
    let days = minutes / (24 * 60);
    let hours = (minutes % (24 * 60)) / 60;
    let minutes = minutes % 60;
    let mut parts = Vec::with_capacity(3);
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    parts.join(" ")
}

fn profile_label(profile: &str) -> String {
    match profile {
        "power-saver" => fl!("power-saver"),
        "balanced" => fl!("balanced"),
        "performance" => fl!("performance"),
        _ => profile.to_string(),
    }
}

fn normalized_profile(profile: &str) -> Option<&'static str> {
    match profile {
        "Battery" | "power-saver" => Some("power-saver"),
        "Balanced" | "balanced" => Some("balanced"),
        "Performance" | "performance" => Some("performance"),
        _ => None,
    }
}

fn network_subscription() -> Subscription<NetworkUpdate> {
    Subscription::run(|| {
        cosmic::iced::stream::channel(
            8,
            |mut output: futures::channel::mpsc::Sender<NetworkUpdate>| async move {
                loop {
                    let Ok(network) = NetworkManager::new().await else {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        continue;
                    };
                    if let Ok(snapshot) = network.snapshot().await {
                        let _ = output.send(network_update(&snapshot)).await;
                    }
                    let Ok(mut events) = network.network_events().await else {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        continue;
                    };
                    while events.next().await.is_some() {
                        if let Ok(snapshot) = network.snapshot().await {
                            let _ = output.send(network_update(&snapshot)).await;
                        }
                    }
                }
            },
        )
    })
}

fn network_update(snapshot: &NetworkSnapshot) -> NetworkUpdate {
    let summary = snapshot.applet_summary();
    let connection_name =
        snapshot
            .active_connections
            .iter()
            .find_map(|connection| match connection {
                ActiveConnection::Wifi(wifi) => Some(wifi.ssid.clone()),
                _ => None,
            });
    NetworkUpdate {
        wifi_enabled: snapshot.wifi.enabled,
        connectivity: format!("{:?}", summary.connectivity.state),
        connection_name,
    }
}

fn bluetooth_subscription() -> Subscription<BluetoothUpdate> {
    Subscription::run(|| {
        cosmic::iced::stream::channel(
            8,
            |mut output: futures::channel::mpsc::Sender<BluetoothUpdate>| async move {
                loop {
                    let Ok(session) = Session::new().await else {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        continue;
                    };
                    let Ok(adapter) = session.default_adapter().await else {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        continue;
                    };
                    let _ = output
                        .send(BluetoothUpdate(adapter.is_powered().await.unwrap_or(false)))
                        .await;
                    let Ok(mut changes) = adapter.events().await else {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        continue;
                    };
                    while let Some(change) = changes.next().await {
                        if let bluer::AdapterEvent::PropertyChanged(AdapterProperty::Powered(
                            powered,
                        )) = change
                        {
                            let _ = output.send(BluetoothUpdate(powered)).await;
                        }
                    }
                }
            },
        )
    })
}

fn audio_subscription() -> Subscription<Message> {
    Subscription::run(|| {
        cosmic::iced::stream::channel(
            16,
            |mut output: futures::channel::mpsc::Sender<Message>| async move {
                loop {
                    let Ok(mut client) = audio_client::connect().await else {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        continue;
                    };
                    let Ok(Ok(mut events)) = client.recv_events().await else {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        continue;
                    };
                    let _ = output.send(Message::AudioClient(Arc::new(client))).await;
                    while let Some(Ok(event)) = events.next().await {
                        let _ = output.send(Message::Audio(event)).await;
                    }
                }
            },
        )
    })
}

async fn set_wifi(enabled: bool) {
    if let Ok(network) = NetworkManager::new().await {
        let _ = network.set_wireless_enabled(enabled).await;
    }
}

async fn set_audio_volume(
    mut client: audio_client::Client,
    pending_volume: Arc<AtomicU32>,
) -> Arc<audio_client::Client> {
    tokio::time::sleep(Duration::from_millis(128)).await;
    let _ = client
        .conn
        .set_sink_volume(pending_volume.load(Ordering::Relaxed))
        .await;
    Arc::new(client)
}

async fn toggle_audio_mute(mut client: audio_client::Client) -> Arc<audio_client::Client> {
    let _ = client.conn.sink_mute_toggle().await;
    Arc::new(client)
}

async fn set_bluetooth(enabled: bool) {
    if let Ok(session) = Session::new().await
        && let Ok(adapter) = session.default_adapter().await
    {
        let _ = adapter.set_powered(enabled).await;
    }
}

async fn get_profile() -> Result<PowerProfile, String> {
    let connection = Connection::system()
        .await
        .map_err(|error| error.to_string())?;

    if let Ok(proxy) = PowerDaemonProxy::new(&connection).await
        && let Ok(active) = proxy.get_profile().await
    {
        return Ok(PowerProfile {
            backend: ProfileBackend::System76,
            active: normalized_profile(&active)
                .ok_or_else(|| format!("unknown System76 power profile: {active}"))?
                .to_string(),
            choices: vec![
                "power-saver".to_string(),
                "balanced".to_string(),
                "performance".to_string(),
            ],
        });
    }

    let proxy = PowerProfilesProxy::new(&connection)
        .await
        .map_err(|error| error.to_string())?;
    let active = proxy
        .active_profile()
        .await
        .map_err(|error| error.to_string())?;
    let choices = proxy
        .profiles()
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter_map(|mut profile| {
            let value = profile.remove("Profile")?;
            let value = String::try_from(value).ok()?;
            normalized_profile(&value).map(str::to_string)
        })
        .collect::<Vec<_>>();
    if choices.is_empty() {
        return Err("UPower exposes no supported power profiles".to_string());
    }
    Ok(PowerProfile {
        backend: ProfileBackend::Upower,
        active: normalized_profile(&active).unwrap_or(&active).to_string(),
        choices,
    })
}

async fn set_profile(profile: PowerProfile) -> Result<PowerProfile, String> {
    let connection = Connection::system()
        .await
        .map_err(|error| error.to_string())?;
    match profile.backend {
        ProfileBackend::System76 => {
            let proxy = PowerDaemonProxy::new(&connection)
                .await
                .map_err(|error| error.to_string())?;
            match profile.active.as_str() {
                "power-saver" => proxy.battery().await,
                "balanced" => proxy.balanced().await,
                "performance" => proxy.performance().await,
                _ => return Err(format!("unsupported power profile: {}", profile.active)),
            }
            .map_err(|error| error.to_string())?;
        }
        ProfileBackend::Upower => {
            let proxy = PowerProfilesProxy::new(&connection)
                .await
                .map_err(|error| error.to_string())?;
            proxy
                .set_active_profile(&profile.active)
                .await
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(profile)
}

fn perform_power_action(action: PowerAction) -> Task<cosmic::Action<Message>> {
    match action {
        PowerAction::Suspend => Task::perform(suspend(), |_| cosmic::Action::None),
        PowerAction::Logout => osd_or_fallback("log-out", log_out),
        PowerAction::Restart => osd_or_fallback("restart", restart),
        PowerAction::Shutdown => osd_or_fallback("shutdown", shutdown),
    }
}

fn osd_or_fallback<F, Fut>(action: &str, fallback: F) -> Task<cosmic::Action<Message>>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = zbus::Result<()>> + Send + 'static,
{
    if std::process::Command::new("cosmic-osd")
        .arg(action)
        .spawn()
        .is_ok()
    {
        Task::none()
    } else {
        Task::perform(fallback(), |_| cosmic::Action::None)
    }
}

async fn suspend() -> zbus::Result<()> {
    let connection = Connection::system().await?;
    ManagerProxy::new(&connection).await?.suspend(true).await
}

async fn restart() -> zbus::Result<()> {
    let connection = Connection::system().await?;
    ManagerProxy::new(&connection).await?.reboot(true).await
}

async fn shutdown() -> zbus::Result<()> {
    let connection = Connection::system().await?;
    ManagerProxy::new(&connection).await?.power_off(true).await
}

async fn log_out() -> zbus::Result<()> {
    let connection = Connection::session().await?;
    if std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref() == Some("pop:GNOME") {
        GnomeSessionManagerProxy::new(&connection)
            .await?
            .logout(0)
            .await
    } else {
        CosmicSessionProxy::new(&connection).await?.exit().await
    }
}

#[zbus::proxy(
    interface = "com.system76.PowerDaemon",
    default_path = "/com/system76/PowerDaemon",
    assume_defaults = true
)]
trait PowerDaemon {
    fn balanced(&self) -> zbus::Result<()>;
    fn battery(&self) -> zbus::Result<()>;
    fn get_profile(&self) -> zbus::Result<String>;
    fn performance(&self) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.UPower.PowerProfiles",
    default_path = "/org/freedesktop/UPower/PowerProfiles",
    assume_defaults = true
)]
trait PowerProfiles {
    #[zbus(property)]
    fn active_profile(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn set_active_profile(&self, value: &str) -> zbus::Result<()>;
    #[zbus(property)]
    fn profiles(&self) -> zbus::Result<Vec<HashMap<String, zbus::zvariant::OwnedValue>>>;
}

#[zbus::proxy(
    interface = "com.system76.CosmicSession",
    default_service = "com.system76.CosmicSession",
    default_path = "/com/system76/CosmicSession"
)]
trait CosmicSession {
    fn exit(&self) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.gnome.SessionManager", assume_defaults = true)]
trait GnomeSessionManager {
    fn logout(&self, mode: u32) -> zbus::Result<()>;
}
