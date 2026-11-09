use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use ab_glyph::{point, Font, FontVec, PxScale, ScaleFont};
use smithay_client_toolkit::reexports::calloop::channel::{self, Sender};
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{EventLoop, LoopHandle};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_shm, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};

const BACKLIGHT: &str = "/sys/class/backlight/intel_backlight";
const W: u32 = 300;
const H: u32 = 64;
const SHOW_MS: u64 = 1500;
const FONT: &str = "/usr/share/fonts/google-noto/NotoSans-Bold.ttf";

#[derive(Clone)]
struct Msg {
    text: String,
    percent: Option<u32>,
}

fn run(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .env("LC_ALL", "C")
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn volume_state() -> (u32, bool) {
    let vol = run("pactl", &["get-sink-volume", "@DEFAULT_SINK@"]);
    let pct = vol
        .split('/')
        .nth(1)
        .and_then(|s| s.trim().trim_end_matches('%').parse().ok())
        .unwrap_or(0);
    let muted = run("pactl", &["get-sink-mute", "@DEFAULT_SINK@"]).contains("yes");
    (pct, muted)
}

fn read_u64(name: &str) -> Option<u64> {
    std::fs::read_to_string(format!("{BACKLIGHT}/{name}"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn spawn_sources(tx: Sender<Msg>) {
    let t = tx.clone();
    thread::spawn(move || {
        let mut last = volume_state();
        loop {
            if let Ok(mut child) = Command::new("pactl")
                .env("LC_ALL", "C")
                .arg("subscribe")
                .stdout(Stdio::piped())
                .spawn()
            {
                if let Some(out) = child.stdout.take() {
                    for line in BufReader::new(out).lines().map_while(Result::ok) {
                        if (line.contains("on sink #") || line.contains("on server")) {
                            let now = volume_state();
                            if now != last {
                                last = now;
                                let m = if now.1 {
                                    Msg { text: "Son coupé".into(), percent: Some(0) }
                                } else {
                                    Msg { text: format!("Volume {} %", now.0), percent: Some(now.0.min(100)) }
                                };
                                let _ = t.send(m);
                            }
                        }
                    }
                }
                let _ = child.wait();
            }
            thread::sleep(Duration::from_secs(2));
        }
    });

    let t = tx.clone();
    thread::spawn(move || {
        let max = read_u64("max_brightness").unwrap_or(0);
        if max == 0 {
            return;
        }
        let mut last = read_u64("brightness");
        loop {
            thread::sleep(Duration::from_millis(150));
            let cur = read_u64("brightness");
            if cur != last {
                if let Some(c) = cur {
                    let p = (c * 100 / max) as u32;
                    let _ = t.send(Msg { text: format!("Luminosité {p} %"), percent: Some(p) });
                }
                last = cur;
            }
        }
    });

    thread::spawn(move || {
        let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
        let path = format!("{dir}/osd-notify.sock");
        let _ = std::fs::remove_file(&path);
        let Ok(listener) = UnixListener::bind(&path) else { return };
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666));
        for stream in listener.incoming().flatten() {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                if !line.is_empty() {
                    let _ = tx.send(Msg { text: line, percent: None });
                }
            }
        }
    });
}

struct App {
    registry: RegistryState,
    output: OutputState,
    shm: Shm,
    compositor: CompositorState,
    layer_shell: LayerShell,
    qh: QueueHandle<App>,
    handle: LoopHandle<'static, App>,
    pool: SlotPool,
    font: FontVec,
    layer: Option<LayerSurface>,
    configured: bool,
    msg: Option<Msg>,
    generation: u64,
    exit: bool,
}

impl App {
    fn show(&mut self, m: Msg) {
        self.msg = Some(m);
        self.generation += 1;
        if self.layer.is_none() {
            let surface = self.compositor.create_surface(&self.qh);
            let layer = self.layer_shell.create_layer_surface(
                &self.qh, surface, Layer::Overlay, Some("osd-notify"), None,
            );
            layer.set_anchor(Anchor::TOP | Anchor::RIGHT);
            layer.set_margin(12, 12, 0, 0);
            layer.set_size(W, H);
            layer.set_exclusive_zone(0);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            if let Ok(region) = Region::new(&self.compositor) {
                layer.wl_surface().set_input_region(Some(region.wl_region()));
            }
            layer.commit();
            self.layer = Some(layer);
            self.configured = false;
        } else if self.configured {
            self.draw();
        }
        let g = self.generation;
        let _ = self.handle.insert_source(
            Timer::from_duration(Duration::from_millis(SHOW_MS)),
            move |_, _, app| {
                if app.generation == g {
                    app.layer = None;
                    app.configured = false;
                }
                TimeoutAction::Drop
            },
        );
    }

    fn draw(&mut self) {
        let (Some(layer), Some(msg)) = (self.layer.as_ref(), self.msg.as_ref()) else { return };
        let stride = W as i32 * 4;
        let Ok((buffer, canvas)) = self.pool.create_buffer(W as i32, H as i32, stride, wl_shm::Format::Argb8888) else { return };

        let mut px = vec![0f32; (W * H * 4) as usize]; // premultiplied RGBA
        let r = 14.0f32;
        for y in 0..H {
            for x in 0..W {
                let cx = (x as f32 + 0.5).clamp(r, W as f32 - r);
                let cy = (y as f32 + 0.5).clamp(r, H as f32 - r);
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let cov = (r - d + 0.5).clamp(0.0, 1.0);
                let a = 0.62 * cov;
                let i = ((y * W + x) * 4) as usize;
                px[i] = 0.14 * a;
                px[i + 1] = 0.15 * a;
                px[i + 2] = 0.16 * a;
                px[i + 3] = a;
            }
        }

        let mut put = |x: u32, y: u32, col: [f32; 3], a: f32| {
            if x < W && y < H && a > 0.0 {
                let i = ((y * W + x) * 4) as usize;
                for k in 0..3 {
                    px[i + k] = col[k] * a + px[i + k] * (1.0 - a);
                }
                px[i + 3] = a + px[i + 3] * (1.0 - a);
            }
        };

        let scale = PxScale::from(20.0);
        let sf = self.font.as_scaled(scale);
        let base_y = if msg.percent.is_some() { 28.0 } else { H as f32 / 2.0 + 7.0 };
        let mut caret = 18.0f32;
        let mut prev = None;
        for ch in msg.text.chars() {
            let gid = self.font.glyph_id(ch);
            if let Some(p) = prev {
                caret += sf.kern(p, gid);
            }
            let glyph = gid.with_scale_and_position(scale, point(caret, base_y));
            caret += sf.h_advance(gid);
            prev = Some(gid);
            if let Some(o) = self.font.outline_glyph(glyph) {
                let b = o.px_bounds();
                o.draw(|gx, gy, c| {
                    let x = b.min.x as i32 + gx as i32;
                    let y = b.min.y as i32 + gy as i32;
                    if x >= 0 && y >= 0 {
                        put(x as u32, y as u32, [0.96, 0.96, 0.97], c);
                    }
                });
            }
        }

        if let Some(p) = msg.percent {
            let (bx, by, bw, bh) = (18u32, 42u32, W - 36, 8u32);
            let fill = bw * p.min(100) / 100;
            let rr = bh as f32 / 2.0;
            for y in 0..bh {
                for x in 0..bw {
                    let cx = (x as f32 + 0.5).clamp(rr, bw as f32 - rr);
                    let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - rr).powi(2)).sqrt();
                    let cov = (rr - d + 0.5).clamp(0.0, 1.0);
                    if x < fill {
                        put(bx + x, by + y, [0.47, 0.66, 0.90], cov);
                    } else {
                        put(bx + x, by + y, [0.35, 0.37, 0.39], cov * 0.7);
                    }
                }
            }
        }

        for (i, chunk) in canvas.chunks_exact_mut(4).enumerate() {
            let s = &px[i * 4..i * 4 + 4];
            chunk[0] = (s[2] * 255.0) as u8;
            chunk[1] = (s[1] * 255.0) as u8;
            chunk[2] = (s[0] * 255.0) as u8;
            chunk[3] = (s[3] * 255.0) as u8;
        }

        layer.wl_surface().damage_buffer(0, 0, W as i32, H as i32);
        let _ = buffer.attach_to(layer.wl_surface());
        layer.commit();
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}
impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}
impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.layer = None;
        self.configured = false;
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface, _: LayerSurfaceConfigure, _: u32) {
        self.configured = true;
        self.draw();
    }
}
impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}
impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry }
    registry_handlers![OutputState];
}
delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_layer!(App);
delegate_registry!(App);

fn main() {
    let conn = Connection::connect_to_env().expect("wayland");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registry");
    let qh = event_queue.handle();
    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("loop");
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), event_queue).insert(handle.clone()).expect("source");

    let compositor = CompositorState::bind(&globals, &qh).expect("compositor");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell");
    let shm = Shm::bind(&globals, &qh).expect("shm");
    let pool = SlotPool::new((W * H * 4) as usize, &shm).expect("pool");
    let font = FontVec::try_from_vec(std::fs::read(FONT).expect("font")).expect("font parse");

    let mut app = App {
        registry: RegistryState::new(&globals),
        output: OutputState::new(&globals, &qh),
        shm, compositor, layer_shell, qh: qh.clone(), handle: handle.clone(), pool, font,
        layer: None, configured: false, msg: None, generation: 0, exit: false,
    };

    let (tx, rx) = channel::channel::<Msg>();
    handle
        .insert_source(rx, |ev, _, app| {
            if let channel::Event::Msg(m) = ev {
                app.show(m);
            }
        })
        .expect("channel");
    spawn_sources(tx);

    while !app.exit {
        event_loop.dispatch(None, &mut app).expect("dispatch");
    }
}
