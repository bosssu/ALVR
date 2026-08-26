// Hide the extra Windows console when launching the mock GUI (release).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use alvr_client_core::{ClientCapabilities, ClientCoreContext, ClientCoreEvent};
use alvr_common::{
    DeviceMotion, Fov, HEAD_ID, Pose, RelaxedAtomic, ViewParams,
    glam::{Quat, UVec2, Vec3},
    parking_lot::RwLock,
};
use alvr_packets::{FaceData, TrackingData};
use alvr_session::CodecType;
use eframe::{
    Frame, NativeOptions, Renderer,
    egui::{self, CentralPanel, IconData, RichText, Slider, Ui, ViewportBuilder},
};
use ico::IconDir;
use serde::{Deserialize, Serialize};
use std::{
    f32::consts::{FRAC_PI_2, PI},
    fs,
    io::Cursor,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct WindowInput {
    height: f32,
    yaw: f32,
    pitch: f32,
    ipd_mm: f32,
    use_random_orientation: bool,
    emulated_decode_ms: u64,
    emulated_compositor_ms: u64,
    emulated_vsync_ms: u64,
    /// Per-eye default/max advertised to the server (32-aligned).
    view_width: u32,
    view_height: u32,
}

impl Default for WindowInput {
    fn default() -> Self {
        Self {
            height: 1.5,
            yaw: 0.0,
            pitch: 0.0,
            ipd_mm: 63.0,
            use_random_orientation: false,
            emulated_decode_ms: 5,
            emulated_compositor_ms: 1,
            emulated_vsync_ms: 25,
            view_width: 1920,
            view_height: 1832,
        }
    }
}

fn align32_u32(v: u32) -> u32 {
    v.max(32) & !31
}

fn settings_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("ALVR").join("client_mock");
    let _ = fs::create_dir_all(&dir);
    dir.join("settings.json")
}

fn load_window_input() -> WindowInput {
    let path = settings_path();
    let Ok(text) = fs::read_to_string(&path) else {
        return WindowInput::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn save_window_input(input: &WindowInput) {
    let path = settings_path();
    if let Ok(text) = serde_json::to_string_pretty(input) {
        let _ = fs::write(path, text);
    }
}

#[derive(Clone)]
struct WindowOutput {
    hud_message: String,
    connected: bool,
    fps: f32,
    resolution: UVec2,
    decoder_codec: Option<CodecType>,
    current_frame_timestamp: Duration,
}

impl Default for WindowOutput {
    fn default() -> Self {
        Self {
            hud_message: "".into(),
            connected: false,
            fps: 1.0,
            resolution: UVec2::ZERO,
            decoder_codec: None,
            current_frame_timestamp: Duration::ZERO,
        }
    }
}

pub struct Window {
    input: WindowInput,
    input_sender: Option<mpsc::Sender<WindowInput>>,
    output: WindowOutput,
    output_receiver: mpsc::Receiver<WindowOutput>,
    shutting_down: Arc<RelaxedAtomic>,
}

impl Window {
    fn new(
        input: WindowInput,
        input_sender: mpsc::Sender<WindowInput>,
        output_receiver: mpsc::Receiver<WindowOutput>,
        shutting_down: Arc<RelaxedAtomic>,
    ) -> Self {
        let _ = input_sender.send(input.clone());
        Self {
            input,
            input_sender: Some(input_sender),
            output: WindowOutput::default(),
            output_receiver,
            shutting_down,
        }
    }

    /// Drop the UI->client channel, then kill the process so the handshake
    /// listener (port 9943) and mdns-sd threads cannot outlive the window.
    fn quit_process(&mut self) {
        self.shutting_down.set(true);
        self.input_sender.take();
        std::process::exit(0);
    }
}

impl eframe::App for Window {
    fn ui(&mut self, ui: &mut Ui, _: &mut Frame) {
        if ui.input(|i| i.viewport().close_requested()) {
            self.quit_process();
        }

        while let Ok(output) = self.output_receiver.try_recv() {
            self.output = output;
        }

        let mut input = self.input.clone();

        CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.heading(RichText::new(&self.output.hud_message));
            });
            ui.label(format!("Connected: {}", self.output.connected));
            ui.label(format!("FPS: {}", self.output.fps));
            ui.label(format!("View resolution: {}", self.output.resolution));
            ui.label(format!(
                "Advertised max (restart mock to apply): {}x{}",
                align32_u32(self.input.view_width),
                align32_u32(self.input.view_height)
            ));
            ui.label(format!("Codec: {:?}", self.output.decoder_codec));
            ui.label(format!(
                "Current frame: {:?}",
                self.output.current_frame_timestamp
            ));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label("View width:");
                ui.add(
                    egui::DragValue::new(&mut input.view_width)
                        .range(32..=8192)
                        .speed(32)
                        .suffix(" px"),
                );
            });
            ui.horizontal(|ui| {
                ui.label("View height:");
                ui.add(
                    egui::DragValue::new(&mut input.view_height)
                        .range(32..=8192)
                        .speed(32)
                        .suffix(" px"),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Height:");
                ui.add(Slider::new(&mut input.height, 0.0..=2.0));
            });
            ui.horizontal(|ui| {
                ui.label("Yaw:");
                ui.add(Slider::new(&mut input.yaw, -PI..=PI));
            });
            ui.horizontal(|ui| {
                ui.label("Pitch:");
                ui.add(Slider::new(&mut input.pitch, -FRAC_PI_2..=FRAC_PI_2));
            });
            ui.horizontal(|ui| {
                ui.label("IPD (mm):");
                ui.add(Slider::new(&mut input.ipd_mm, 50.0..=80.0));
            });
            ui.checkbox(
                &mut input.use_random_orientation,
                "Use randomized orientation offset",
            );
        });

        if input != self.input {
            input.view_width = align32_u32(input.view_width);
            input.view_height = align32_u32(input.view_height);
            self.input = input;
            save_window_input(&self.input);
            if let Some(sender) = &self.input_sender {
                sender.send(self.input.clone()).ok();
            }
        }

        ui.request_repaint();
    }
}

fn stereo_view_params(ipd_m: f32) -> [ViewParams; 2] {
    let half = ipd_m * 0.5;
    let fov = Fov::DUMMY;
    [
        ViewParams {
            pose: Pose {
                orientation: Quat::IDENTITY,
                position: Vec3::new(-half, 0.0, 0.0),
            },
            fov,
        },
        ViewParams {
            pose: Pose {
                orientation: Quat::IDENTITY,
                position: Vec3::new(half, 0.0, 0.0),
            },
            fov,
        },
    ]
}

fn tracking_thread(
    context: Arc<ClientCoreContext>,
    streaming: Arc<RelaxedAtomic>,
    fps: f32,
    input: Arc<RwLock<WindowInput>>,
) {
    let timestamp_origin = Instant::now();
    let mut last_ipd_m = -1.0_f32;
    let mut ticks = 0_u32;
    context.send_proximity_state(true);

    let mut loop_deadline = Instant::now();
    while streaming.value() {
        let input_lock = input.read();
        let ipd_m = (input_lock.ipd_mm / 1000.0).clamp(0.04, 0.09);
        ticks = ticks.saturating_add(1);
        if ticks < 30 || (ipd_m - last_ipd_m).abs() > 0.0005 {
            context.send_view_params(stereo_view_params(ipd_m));
            last_ipd_m = ipd_m;
        }

        let mut orientation =
            Quat::from_rotation_y(input_lock.yaw) * Quat::from_rotation_x(input_lock.pitch);

        if input_lock.use_random_orientation {
            orientation *= Quat::from_rotation_z(rand::random::<f32>() * 0.001);
        }

        let position = Vec3::new(0.0, input_lock.height, 0.0);

        context.send_tracking(TrackingData {
            poll_timestamp: timestamp_origin.elapsed(),
            device_motions: vec![(
                *HEAD_ID,
                DeviceMotion {
                    pose: Pose {
                        orientation,
                        position,
                    },
                    linear_velocity: Vec3::ZERO,
                    angular_velocity: Vec3::ZERO,
                },
            )],
            hand_skeletons: [None, None],
            face: FaceData::default(),
            body: None,
        });

        drop(input_lock);

        loop_deadline += Duration::from_secs_f32(1.0 / fps / 3.0);
        thread::sleep(loop_deadline.saturating_duration_since(Instant::now()))
    }
}

fn client_thread(
    output_sender: mpsc::Sender<WindowOutput>,
    input_receiver: mpsc::Receiver<WindowInput>,
    initial_input: WindowInput,
    shutting_down: Arc<RelaxedAtomic>,
) {
    let view = UVec2::new(
        align32_u32(initial_input.view_width),
        align32_u32(initial_input.view_height),
    );
    let capabilities = ClientCapabilities {
        platform: alvr_system_info::platform(None, None),
        default_view_resolution: view,
        max_view_resolution: view,
        refresh_rates: vec![60.0, 72.0, 80.0, 90.0, 120.0],
        foveated_encoding: false,
        encoder_high_profile: false,
        encoder_10_bits: false,
        encoder_av1: false,
        prefer_10bit: false,
        preferred_encoding_gamma: 1.0,
        prefer_hdr: false,
    };
    let client_core_context = Arc::new(ClientCoreContext::new(capabilities));

    client_core_context.resume();

    let streaming = Arc::new(RelaxedAtomic::new(false));
    let got_decoder_config = Arc::new(RelaxedAtomic::new(false));
    let mut maybe_tracking_thread = None;

    let mut window_output = WindowOutput::default();
    let window_input = Arc::new(RwLock::new(initial_input));

    let mut deadline = Instant::now();
    'main_loop: loop {
        if shutting_down.value() {
            break 'main_loop;
        }

        let input_lock = window_input.read();

        while let Some(event) = client_core_context.poll_event() {
            match event {
                ClientCoreEvent::UpdateHudMessage(message) => {
                    window_output.hud_message = message;
                }
                ClientCoreEvent::StreamingStarted(config) => {
                    window_output.fps = config.negotiated_config.refresh_rate_hint;
                    window_output.connected = true;
                    window_output.resolution = config.negotiated_config.view_resolution;

                    streaming.set(true);

                    let context = Arc::clone(&client_core_context);
                    let streaming = Arc::clone(&streaming);
                    let input = Arc::clone(&window_input);
                    maybe_tracking_thread = Some(thread::spawn(move || {
                        tracking_thread(
                            context,
                            streaming,
                            config.negotiated_config.refresh_rate_hint,
                            input,
                        )
                    }));
                }
                ClientCoreEvent::StreamingStopped => {
                    streaming.set(false);
                    got_decoder_config.set(false);

                    if let Some(thread) = maybe_tracking_thread.take() {
                        thread.join().ok();
                    }

                    window_output.fps = 1.0;
                    window_output.connected = false;
                    window_output.resolution = UVec2::ZERO;
                    window_output.decoder_codec = None;
                }
                ClientCoreEvent::DecoderConfig { codec, .. } => {
                    got_decoder_config.set(true);

                    window_output.decoder_codec = Some(codec);
                }
                ClientCoreEvent::Haptics { .. } | ClientCoreEvent::RealTimeConfig(_) => (),
            }

            output_sender.send(window_output.clone()).ok();
        }

        thread::sleep(Duration::from_millis(3));

        client_core_context.report_compositor_start(window_output.current_frame_timestamp);

        thread::sleep(Duration::from_millis(input_lock.emulated_compositor_ms));

        client_core_context.report_submit(
            window_output.current_frame_timestamp,
            Duration::from_millis(input_lock.emulated_vsync_ms),
        );

        drop(input_lock);

        match input_receiver.try_recv() {
            Ok(input) => *window_input.write() = input,
            Err(TryRecvError::Disconnected) => break 'main_loop,
            Err(TryRecvError::Empty) => (),
        }

        deadline += Duration::from_secs_f32(1.0 / window_output.fps);
        thread::sleep(deadline.saturating_duration_since(Instant::now()));
    }

    streaming.set(false);
    if let Some(thread) = maybe_tracking_thread {
        thread.join().ok();
    }

    // Do not call pause(): it waits on disconnected_notif and can hang while the
    // handshake listener still owns CONTROL_PORT (9943). Dropping the context
    // sets ShuttingDown and joins the connection thread instead.
}

fn main() {
    env_logger::init();

    let initial_input = load_window_input();

    let (input_sender, input_receiver) = mpsc::channel::<WindowInput>();
    let (output_sender, output_receiver) = mpsc::channel::<WindowOutput>();
    let shutting_down = Arc::new(RelaxedAtomic::new(false));

    let client_thread = thread::spawn({
        let initial_input = initial_input.clone();
        let shutting_down = Arc::clone(&shutting_down);
        move || {
            client_thread(
                output_sender,
                input_receiver,
                initial_input,
                shutting_down,
            );
        }
    });

    let ico = IconDir::read(Cursor::new(include_bytes!("../resources/client_mock.ico"))).unwrap();
    let image = ico
        .entries()
        .iter()
        .max_by_key(|e| e.width() as u32 * e.height() as u32)
        .unwrap()
        .decode()
        .unwrap();

    eframe::run_native(
        "Mock client",
        NativeOptions {
            viewport: ViewportBuilder::default()
                .with_inner_size((420.0, 480.0))
                .with_icon(IconData {
                    rgba: image.rgba_data().to_owned(),
                    width: image.width(),
                    height: image.height(),
                }),
            renderer: Renderer::Glow,
            ..Default::default()
        },
        Box::new(move |_| {
            Ok(Box::new(Window::new(
                initial_input,
                input_sender,
                output_receiver,
                shutting_down,
            )))
        }),
    )
    .ok();

    client_thread.join().unwrap();
}
