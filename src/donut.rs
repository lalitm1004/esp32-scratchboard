//! 3D spinning torus renderer.
//!
//! Renders the classic "donut" — a shaded torus rotating in 3D — into a
//! 240x320 Rgb565 framebuffer. A packed depth value is kept alongside the
//! color so nearer surface points correctly occlude the far side of the tube.

use embedded_graphics_core::pixelcolor::{Rgb565, RgbColor};

pub const WIDTH: usize = 240;
pub const HEIGHT: usize = 320;

const N: usize = WIDTH * HEIGHT;

const TAU: f32 = core::f32::consts::TAU;

// Torus geometry.
const R1: f32 = 1.0; // major radius
const R2: f32 = 0.42; // minor (tube) radius

// Perspective projection.
const ZOOM: f32 = 90.0; // focal length, in pixels
const DIST: f32 = 2.0; // camera distance

// Rotation speeds, radians per frame. Crank these up for a faster spin;
// past ~0.6 rad/frame the donut starts to strobe/blur between frames.
const A_SPEED: f32 = 0.45; // spin around X
const B_SPEED: f32 = 0.22; // tilt around Z
const C_SPEED: f32 = 0.18; // light orbit

// Angular resolution (points sampled per ring / per tube circumference).
const THETA: usize = 150;
const PHI: usize = 110;

// `zoom / (dist + z)` is monotonic with closeness, so a plain unsigned
// greater-than comparison gives correct hidden-surface removal.
//
// Precomputed trigonometry for the fixed theta/phi grids. This never changes
// between frames, so it's built once.
struct Tables {
    cos_theta: [f32; THETA],
    sin_theta: [f32; THETA],
    cos_phi: [f32; PHI],
    sin_phi: [f32; PHI],
    // R1 + R2*cos(phi): tube-center radius at this phi.
    ring: [f32; PHI],
    // R2*sin(phi): z of the tube-center at this phi.
    z: [f32; PHI],
}

pub struct Donut {
    tables: Tables,
    a: f32,
    b: f32,
    c: f32,
}

impl Donut {
    pub fn new() -> Self {
        let mut tables = Tables {
            cos_theta: [0.0; THETA],
            sin_theta: [0.0; THETA],
            cos_phi: [0.0; PHI],
            sin_phi: [0.0; PHI],
            ring: [0.0; PHI],
            z: [0.0; PHI],
        };

        for k in 0..THETA {
            let th = k as f32 / THETA as f32 * TAU;
            tables.cos_theta[k] = libm::cosf(th);
            tables.sin_theta[k] = libm::sinf(th);
        }

        for j in 0..PHI {
            let ph = j as f32 / PHI as f32 * TAU;
            tables.cos_phi[j] = libm::cosf(ph);
            tables.sin_phi[j] = libm::sinf(ph);
            tables.ring[j] = R1 + R2 * tables.cos_phi[j];
            tables.z[j] = R2 * tables.sin_phi[j];
        }

        Self {
            tables,
            a: 0.0,
            b: 0.0,
            c: 0.0,
        }
    }

    /// Advance the rotation and render one frame into `color` / `depth`.
    pub fn render(&mut self, color: &mut [Rgb565; N], depth: &mut [u8; N]) {
        color.fill(Rgb565::BLACK);
        depth.fill(0);

        let sin_a = libm::sinf(self.a);
        let cos_a = libm::cosf(self.a);
        let sin_b = libm::sinf(self.b);
        let cos_b = libm::cosf(self.b);
        let sin_c = libm::sinf(self.c);
        let cos_c = libm::cosf(self.c);

        // Orbiting light direction, unit length (0.6^2 + 0.8^2 = 1).
        let lx = 0.6 * cos_c;
        let ly = 0.6 * sin_c;
        let lz = 0.8;

        let cx = WIDTH as f32 * 0.5;
        let cy = HEIGHT as f32 * 0.5;

        let t = &self.tables;

        for k in 0..THETA {
            let ct = t.cos_theta[k];
            let st = t.sin_theta[k];

            for j in 0..PHI {
                // Object-space torus point.
                let px = t.ring[j] * ct;
                let py = t.ring[j] * st;
                let pz = t.z[j];

                // Object-space surface normal (unit length).
                let nx = t.cos_phi[j] * ct;
                let ny = t.cos_phi[j] * st;
                let nz = t.sin_phi[j];

                // Rotate around X by `a`, then around Z by `b`.
                let py1 = py * cos_a - pz * sin_a;
                let pz1 = py * sin_a + pz * cos_a;
                let ny1 = ny * cos_a - nz * sin_a;
                let nz1 = ny * sin_a + nz * cos_a;

                let px2 = px * cos_b - py1 * sin_b;
                let py2 = px * sin_b + py1 * cos_b;
                let nx2 = nx * cos_b - ny1 * sin_b;
                let ny2 = nx * sin_b + ny1 * cos_b;

                // Perspective project.
                let inv = ZOOM / (DIST + pz1);
                let sx = cx + px2 * inv;
                let sy = cy + py2 * inv;

                if sx < 0.0 || sx >= WIDTH as f32 || sy < 0.0 || sy >= HEIGHT as f32 {
                    continue;
                }

                // Depth: `inv` ~ [20, 57], larger == closer. Scale to u8
                // (0 = empty) and keep the unsigned greater-than comparison.
                let zval = (inv * 4.0) as u8;
                let idx = (sy as usize) * WIDTH + (sx as usize);

                if zval > depth[idx] {
                    depth[idx] = zval;

                    // Lambert shading with a touch of ambient.
                    let lum = (nx2 * lx + ny2 * ly + nz1 * lz).max(0.0);
                    let lum = 0.12 + 0.88 * lum;
                    color[idx] = shade(lum);
                }
            }
        }

        self.a += A_SPEED;
        self.b += B_SPEED;
        self.c += C_SPEED;
        if self.a > TAU {
            self.a -= TAU;
        }
        if self.b > TAU {
            self.b -= TAU;
        }
        if self.c > TAU {
            self.c -= TAU;
        }
    }
}

/// Map a luminance in [0, 1] to an Rgb565 color along a warm glaze gradient.
fn shade(lum: f32) -> Rgb565 {
    let lum = lum.clamp(0.0, 1.0);

    // Dark maroon -> bright pink/white highlight.
    let r = 20.0 + 230.0 * lum;
    let g = 5.0 + 200.0 * lum;
    let b = 20.0 + 180.0 * lum;

    Rgb565::new(
        (r * 31.0 / 255.0) as u8,
        (g * 63.0 / 255.0) as u8,
        (b * 31.0 / 255.0) as u8,
    )
}
