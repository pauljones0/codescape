//! Scripted camera flight through the repository, used for demos and
//! recordings (`--tour`, `T`).

use crate::camera::{v3, Camera, V3};
use crate::layout::{Layout, COL_CHARS, LINE_H};
use crate::scene::overview;

pub struct Shot {
    cam: Camera,
    travel: f32,
    hold: f32,
    /// Camera drift per second while holding.
    drift: V3,
    yaw_drift: f32,
    caption: Option<String>,
}

pub struct Tour {
    shots: Vec<Shot>,
    from: Camera,
    index: usize,
    t: f64,
    looped: bool,
}

/// Screen height the tour's reading distance is tuned for.
const REF_HEIGHT: f32 = 1080.0;

fn dir_view(l: &Layout, path: &str) -> Option<Camera> {
    let d = &l.dirs[l.find_dir(path)?];
    let (cx, cz) = d.rect.center();
    Some(Camera {
        target: v3(cx, d.y_top, cz + d.rect.h * 0.05),
        dist: d.rect.w.max(d.rect.h * 1.3) * 0.95,
        yaw: -0.18,
        pitch: 0.92,
    })
}

fn file_view(l: &Layout, path: &str, sources_paths: &[&str]) -> Option<(Camera, usize)> {
    let i = l.files.iter().position(|f| sources_paths[f.file] == path)?;
    let f = &l.files[i];
    let focal = REF_HEIGHT * 0.5 / (crate::camera::FOV_Y * 0.5).tan();
    let dist = LINE_H * focal / 15.0;
    let cam = Camera {
        target: v3(
            f.tile.x + COL_CHARS as f32 * 0.42,
            f.y,
            f.tile.z + dist * 0.28,
        ),
        dist,
        yaw: 0.0,
        pitch: 1.12,
    };
    Some((cam, i))
}

impl Tour {
    pub fn for_map(l: &Layout, width: f32, height: f32, looped: bool) -> Tour {
        let home = Camera::fit_map(l.dirs[0].rect, width, height);
        let mut dirs = l.dirs[0].dirs.clone();
        dirs.sort_by_key(|&i| std::cmp::Reverse(l.dirs[i].lines));
        let mut shots = vec![Shot {
            cam: home,
            travel: 0.0,
            hold: 2.0,
            drift: V3::default(),
            yaw_drift: 0.0,
            caption: None,
        }];
        for &i in dirs.iter().take(5) {
            let dir = &l.dirs[i];
            shots.push(Shot {
                cam: Camera::fit_map(dir.rect, width, height),
                travel: 1.5,
                hold: 2.0,
                drift: V3::default(),
                yaw_drift: 0.0,
                caption: Some(format!("{}/", dir.path)),
            });
        }
        if let Some((i, f)) = l.files.iter().enumerate().max_by_key(|(_, f)| f.lines) {
            shots.push(Shot {
                cam: crate::scene::map_reading_view(l, i, width, height),
                travel: 1.5,
                hold: 3.0,
                drift: V3::default(),
                yaw_drift: 0.0,
                caption: Some(f.name.clone()),
            });
        }
        shots.push(Shot {
            cam: home,
            travel: 1.5,
            hold: 1.0,
            drift: V3::default(),
            yaw_drift: 0.0,
            caption: None,
        });
        Tour {
            from: home,
            shots,
            index: 0,
            t: 0.0,
            looped,
        }
    }

    pub fn for_repo(l: &Layout, paths: &[&str], looped: bool) -> Tour {
        let home = overview(l);
        let mut shots = vec![Shot {
            cam: home,
            travel: 0.0,
            hold: 3.0,
            drift: v3(0.0, 0.0, 0.0),
            yaw_drift: 0.02,
            caption: Some(format!(
                "{}: {} files, {:.2}M lines",
                l.dirs[0].name,
                l.files.len(),
                l.total_lines as f64 / 1e6
            )),
        }];
        let dir = |shots: &mut Vec<Shot>, path: &str, caption: &str| {
            if let Some(cam) = dir_view(l, path) {
                shots.push(Shot {
                    cam,
                    travel: 3.2,
                    hold: 1.8,
                    drift: v3(0.0, 0.0, 0.0),
                    yaw_drift: 0.025,
                    caption: Some(caption.to_string()),
                });
            }
        };
        let file = |shots: &mut Vec<Shot>, path: &str, caption: &str| {
            if let Some((cam, f)) = file_view(l, path, paths) {
                let pan = (l.files[f].lines_per_col as f32 * LINE_H).min(160.0);
                shots.push(Shot {
                    cam,
                    travel: 3.6,
                    hold: 5.5,
                    drift: v3(0.0, 0.0, pan / 5.5),
                    yaw_drift: 0.0,
                    caption: Some(caption.to_string()),
                });
            }
        };
        dir(
            &mut shots,
            "pallets",
            "pallets/  Substrate FRAME runtime + consensus",
        );
        if let Some(d) = l.find_dir("pallets") {
            // Low pass across the district so the blocks read as terrain.
            let r = l.dirs[d].rect;
            let hold = 5.0;
            shots.push(Shot {
                cam: Camera {
                    target: v3(r.x + r.w * 0.25, l.dirs[d].y_top, r.z + r.h * 0.45),
                    dist: r.w * 0.32,
                    yaw: 0.55,
                    pitch: 0.42,
                },
                travel: 3.0,
                hold,
                drift: v3(r.w * 0.45 / hold, 0.0, r.h * 0.1 / hold),
                yaw_drift: -0.06,
                caption: Some("every block is a directory, every tile a file".to_string()),
            });
        }
        file(
            &mut shots,
            "pallets/pallets/has-station/src/lib.rs",
            "has-station: HTLC + policy escrow pallet",
        );
        dir(&mut shots, "sdk", "sdk/  TypeScript, Python and agent SDKs");
        file(
            &mut shots,
            "sdk/typescript/src/x402.ts",
            "x402.ts: pay-per-request inference",
        );
        dir(
            &mut shots,
            "formal-specs",
            "formal-specs/  Quint + Lean models",
        );
        dir(&mut shots, "explorer", "explorer/  Next.js block explorer");
        let mut low = home;
        low.pitch = 0.55;
        low.yaw = 0.5;
        low.dist *= 0.8;
        shots.push(Shot {
            cam: low,
            travel: 4.0,
            hold: 2.5,
            drift: v3(0.0, 0.0, 0.0),
            yaw_drift: 0.05,
            caption: None,
        });
        Tour {
            from: home,
            shots,
            index: 0,
            t: 0.0,
            looped,
        }
    }

    pub fn start_from(&mut self, cam: Camera) {
        self.from = cam;
        self.index = 0;
        self.t = 0.0;
        self.shots[0].travel = 2.5;
    }

    pub fn caption(&self) -> Option<&str> {
        let s = self.shots.get(self.index)?;
        // Show the caption once the camera is most of the way there.
        (self.t > f64::from(s.travel) * 0.55)
            .then_some(s.caption.as_deref())
            .flatten()
    }

    /// Advances by `dt` seconds; returns the camera, or None when finished.
    pub fn advance(&mut self, dt: f64) -> Option<Camera> {
        self.t += dt;
        loop {
            let s = self.shots.get(self.index)?;
            let duration = f64::from(s.travel) + f64::from(s.hold);
            if self.t < duration {
                break;
            }
            self.t -= duration;
            self.from = held(s, s.hold);
            self.index += 1;
            if self.index == self.shots.len() {
                if !self.looped {
                    return None;
                }
                self.index = 0;
                self.shots[0].travel = 4.0;
                // A screenshot can seek arbitrarily far into a looping tour.
                let cycle = self
                    .shots
                    .iter()
                    .map(|s| f64::from(s.travel) + f64::from(s.hold))
                    .sum::<f64>();
                self.t %= cycle;
            }
        }
        let s = &self.shots[self.index];
        let travel = f64::from(s.travel);
        if self.t < travel {
            let u = (self.t / travel) as f32;
            let e = u * u * (3.0 - 2.0 * u);
            let mut c = self.from.lerp(&s.cam, e);
            // Rise on long hops so the flight reads as a flight.
            let hop = self.from.target.sub(s.cam.target).len();
            let low = self.from.dist.min(s.cam.dist);
            let lift = (hop / low).max(1.0).ln() * 0.55;
            c.dist *= (lift * (std::f32::consts::PI * e).sin()).exp();
            Some(c)
        } else {
            Some(held(s, (self.t - travel) as f32))
        }
    }
}

fn held(s: &Shot, t: f32) -> Camera {
    let mut c = s.cam;
    c.target = c.target.add(s.drift.scale(t));
    c.yaw += s.yaw_drift * t;
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looping_tour_skips_complete_cycles_when_seeking() {
        let home = Camera {
            target: V3::default(),
            dist: 1.0,
            yaw: 0.0,
            pitch: 0.0,
        };
        let mut tour = Tour {
            shots: vec![Shot {
                cam: home,
                travel: 0.0,
                hold: 2.0,
                drift: V3::default(),
                yaw_drift: 0.0,
                caption: None,
            }],
            from: home,
            index: 0,
            t: 0.0,
            looped: true,
        };
        assert!(tour.advance(6.0 * 1_000_000_000.0 + 3.25).is_some());
        assert_eq!(tour.t, 1.25);
        assert!(tour.advance(1e300).is_some());
        assert!((0.0..6.0).contains(&tour.t));
    }
}
