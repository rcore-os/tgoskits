//! Model-independent analog pin -> selector/mixer -> output-converter walk.
use alloc::vec::Vec;

use crate::{Error, Result};

const MAX_AMP_CONNECTIONS: usize = 16;

pub trait Verbs {
    fn verb(&mut self, codec: u8, node: u8, operation: u16, payload: u16) -> Result<u32>;
}
#[derive(Clone, Debug)]
pub struct Widget {
    pub node: u8,
    pub caps: u32,
    pub pin_caps: u32,
    pub config: u32,
    pub connections: Vec<u8>,
}
impl Widget {
    fn kind(&self) -> u32 {
        (self.caps >> 20) & 15
    }
}
#[derive(Clone, Debug)]
pub struct Route {
    pub codec: u8,
    pub function: u8,
    pub vendor: u32,
    pub path: Vec<Widget>,
}
fn parameter(v: &mut impl Verbs, c: u8, n: u8, p: u16) -> Result<u32> {
    v.verb(c, n, 0xf00, p)
}
fn children(v: &mut impl Verbs, c: u8, n: u8) -> Result<core::ops::Range<u16>> {
    let value = parameter(v, c, n, 4)?;
    let start = ((value >> 16) & 255) as u16;
    let count = (value & 255) as u16;
    if start + count > 128 {
        return Err(Error::InvalidParam);
    }
    Ok(start..start + count)
}
fn connections(v: &mut impl Verbs, c: u8, n: u8) -> Result<Vec<u8>> {
    let format = parameter(v, c, n, 0x0e)?;
    let count = (format & 127) as usize;
    let long = format & 128 != 0;
    let per = if long { 2 } else { 4 };
    let bits = if long { 16 } else { 8 };
    let mut list = Vec::new();
    for base in (0..count).step_by(per) {
        let value = v.verb(c, n, 0xf02, base as u16)?;
        for index in 0..per.min(count - base) {
            let raw = (value >> (index * bits)) & if long { 65535 } else { 255 };
            let range = raw & if long { 32768 } else { 128 } != 0;
            let target = raw & if long { 32767 } else { 127 };
            if target == 0 || target > 127 {
                return Err(Error::InvalidParam);
            }
            if range {
                let previous = *list.last().ok_or(Error::InvalidParam)?;
                if target <= u32::from(previous) {
                    return Err(Error::InvalidParam);
                }
                for node in previous + 1..=target as u8 {
                    list.push(node);
                }
            } else {
                list.push(target as u8);
            }
            if list.len() > 32 {
                return Err(Error::Unsupported);
            }
        }
    }
    Ok(list)
}

fn read_widgets(v: &mut impl Verbs, codec: u8, function: u8) -> Result<Vec<Widget>> {
    let mut nodes = Vec::new();
    for node in children(v, codec, function)? {
        let n = node as u8;
        let caps = parameter(v, codec, n, 9)?;
        let pin = (caps >> 20) & 15 == 4;
        nodes.push(Widget {
            node: n,
            caps,
            pin_caps: if pin { parameter(v, codec, n, 0xc)? } else { 0 },
            config: if pin { v.verb(codec, n, 0xf1c, 0)? } else { 0 },
            connections: if caps & (1 << 8) != 0 {
                connections(v, codec, n)?
            } else {
                Vec::new()
            },
        });
    }
    Ok(nodes)
}
fn walk(nodes: &[Widget], node: u8, visited: &mut [bool; 256], path: &mut Vec<Widget>) -> bool {
    if visited[usize::from(node)] || path.len() == 32 {
        return false;
    }
    visited[usize::from(node)] = true;
    let Some(widget) = nodes.iter().find(|w| w.node == node) else {
        return false;
    };
    if widget.caps & (1 << 9) != 0 {
        return false;
    }
    path.push(widget.clone());
    if widget.kind() == 0 {
        return true;
    }
    if [2, 3, 4].contains(&widget.kind()) {
        for target in &widget.connections {
            if walk(nodes, *target, visited, path) {
                return true;
            }
        }
    }
    path.pop();
    false
}
pub fn find_route(nodes: &[Widget]) -> Option<Vec<Widget>> {
    // Prefer headphones, then speakers/line out, never a digital pin.
    for device in [2, 1, 0] {
        for pin in nodes.iter().filter(|w| {
            w.kind() == 4
                && w.pin_caps & 16 != 0
                && (w.config >> 20) & 15 == device
                && w.config >> 30 != 1
        }) {
            let mut path = Vec::new();
            if walk(nodes, pin.node, &mut [false; 256], &mut path) {
                return Some(path);
            }
        }
    }
    None
}

pub fn enumerate(v: &mut impl Verbs, present: u16) -> Result<Route> {
    for c in 0..15 {
        if present & (1 << c) == 0 {
            continue;
        }
        let vendor = parameter(v, c, 0, 0)?;
        for function in children(v, c, 0)? {
            let f = function as u8;
            if parameter(v, c, f, 5)? & 255 != 1 {
                continue;
            }
            let nodes = read_widgets(v, c, f)?;
            if let Some(path) = find_route(&nodes) {
                return Ok(Route {
                    codec: c,
                    function: f,
                    vendor,
                    path,
                });
            }
        }
    }
    Err(Error::Unsupported)
}

fn unmute(
    v: &mut impl Verbs,
    c: u8,
    node: u8,
    input: bool,
    index: u16,
    function: u8,
    override_caps: bool,
) -> Result {
    let caps = parameter(
        v,
        c,
        if override_caps { node } else { function },
        if input { 0xd } else { 0x12 },
    )?;
    let gain = (caps & 127).min((caps >> 8) & 127) as u16;
    v.verb(
        c,
        node,
        0x300,
        (if input { 0x4000 } else { 0x8000 }) | 0x3000 | (index << 8) | gain,
    )?;
    Ok(())
}
pub fn configure(v: &mut impl Verbs, route: &Route) -> Result {
    for widget in &route.path {
        if widget.caps & 2 != 0
            && [2, 3].contains(&widget.kind())
            && widget.connections.len() > MAX_AMP_CONNECTIONS
        {
            // Set Amp's per-connection index is only four bits wide.
            return Err(Error::Unsupported);
        }
    }

    let c = route.codec;
    v.verb(c, route.function, 0x705, 0)?;
    for (index, w) in route.path.iter().enumerate() {
        if w.caps & (1 << 10) != 0 {
            v.verb(c, w.node, 0x705, 0)?;
        }
        if let Some(next) = route.path.get(index + 1) {
            let selected = w
                .connections
                .iter()
                .position(|n| *n == next.node)
                .ok_or(Error::BadState)?;
            if w.kind() != 2 && w.connections.len() > 1 {
                v.verb(c, w.node, 0x701, selected as u16)?;
            }
            if w.caps & 2 != 0 {
                for i in 0..w.connections.len() {
                    if i != selected {
                        v.verb(c, w.node, 0x300, 0x7080 | ((i as u16) << 8))?;
                    }
                }
                unmute(
                    v,
                    c,
                    w.node,
                    true,
                    selected as u16,
                    route.function,
                    w.caps & 8 != 0,
                )?;
            }
        }
        if w.caps & 4 != 0 {
            unmute(v, c, w.node, false, 0, route.function, w.caps & 8 != 0)?;
        }
        if w.kind() == 4 {
            v.verb(
                c,
                w.node,
                0x707,
                0x40 | if w.pin_caps & 8 != 0 { 0x80 } else { 0 },
            )?;
            if w.pin_caps & (1 << 16) != 0 {
                v.verb(c, w.node, 0x70c, 2)?;
            }
        }
        if w.kind() == 0 {
            v.verb(c, w.node, 0x200, crate::desc::FORMAT)?;
            v.verb(c, w.node, 0x706, 0x10)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RecordingVerbs(Vec<(u8, u8, u16, u16)>);

    impl Verbs for RecordingVerbs {
        fn verb(&mut self, codec: u8, node: u8, operation: u16, payload: u16) -> Result<u32> {
            self.0.push((codec, node, operation, payload));
            Ok(0)
        }
    }

    #[test]
    fn configuration_rejects_input_amp_lists_wider_than_the_verb_index() {
        let route = Route {
            codec: 0,
            function: 1,
            vendor: 0,
            path: alloc::vec![
                Widget {
                    node: 1,
                    caps: (4 << 20) | (1 << 8),
                    pin_caps: 1 << 4,
                    config: 0,
                    connections: alloc::vec![2],
                },
                Widget {
                    node: 2,
                    caps: (3 << 20) | (1 << 1) | (1 << 8),
                    pin_caps: 0,
                    config: 0,
                    connections: alloc::vec![
                        4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 3,
                    ],
                },
                Widget {
                    node: 3,
                    caps: 1,
                    pin_caps: 0,
                    config: 0,
                    connections: alloc::vec![],
                },
            ],
        };
        let mut verbs = RecordingVerbs(alloc::vec::Vec::new());

        assert_eq!(configure(&mut verbs, &route), Err(Error::Unsupported));
        assert!(
            verbs.0.is_empty(),
            "reject before partially programming the route"
        );
    }
}
