//! Current _PRT routes, including already-programmed PCI interrupt links.
use alloc::{format, string::String, vec::Vec};

use crate::{BAD_PARAMETER, Engine, NO_MEMORY, SUPPORT, Status, Value};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub device: u16,
    pub function: u16,
    pub pin: u8,
    pub gsi: u32,
    pub active_low: bool,
}
/// A firmware-described bus; parent is the upstream bridge BDF.
pub struct Scope {
    pub number: u8,
    pub parent: Option<(u8, u8, u8)>,
    pub routes: Vec<Route>,
}
/// Resolve exact/wildcard functions, then swizzle only across scopes lacking
/// a _PRT. A missing entry in a populated _PRT is an explicit routing failure.
pub fn resolve(
    scopes: &[Scope],
    mut bus: u8,
    mut device: u8,
    mut function: u8,
    pin: u8,
) -> Option<(u32, bool)> {
    if !(1..=4).contains(&pin) {
        return None;
    }
    let mut pin = pin - 1;
    for _ in 0..64 {
        let scope = scopes.iter().find(|s| s.number == bus)?;
        let matches = |r: &&Route| r.device == u16::from(device) && r.pin == pin;
        let route = scope
            .routes
            .iter()
            .filter(matches)
            .find(|r| r.function == u16::from(function))
            .or_else(|| {
                scope
                    .routes
                    .iter()
                    .filter(matches)
                    .find(|r| r.function == 0xffff)
            });
        if let Some(r) = route {
            return Some((r.gsi, r.active_low));
        }
        if !scope.routes.is_empty() {
            return None;
        }
        let parent = scope.parent?;
        pin = (pin + device) & 3;
        (bus, device, function) = parent;
    }
    None
}
pub fn parse(
    value: Value,
    mut link: impl FnMut(&str, u32) -> Result<(u32, bool), Status>,
) -> Result<Vec<Route>, Status> {
    let Value::Package(entries) = value else {
        return Err(BAD_PARAMETER);
    };
    let mut routes = Vec::new();
    routes
        .try_reserve_exact(entries.len())
        .map_err(|_| NO_MEMORY)?;
    for entry in entries {
        let Value::Package(v) = entry else {
            return Err(BAD_PARAMETER);
        };
        if v.len() != 4 {
            return Err(BAD_PARAMETER);
        }
        let (Value::Integer(address), Value::Integer(pin), source, Value::Integer(index)) =
            (&v[0], &v[1], &v[2], &v[3])
        else {
            return Err(BAD_PARAMETER);
        };
        if *address > u32::MAX.into()
            || address >> 16 > 31
            || !(address & 0xffff == 0xffff || address & 0xffff <= 7)
            || *pin > 3
            || *index > u32::MAX.into()
        {
            return Err(BAD_PARAMETER);
        }
        let (gsi, active_low) = match source {
            Value::Integer(0) => (*index as u32, true),
            Value::String(path) if path.is_empty() => (*index as u32, true),
            Value::String(path) | Value::Reference(path) => link(
                core::str::from_utf8(path).map_err(|_| BAD_PARAMETER)?,
                *index as u32,
            )?,
            _ => return Err(BAD_PARAMETER),
        };
        let route = Route {
            device: (address >> 16) as u16,
            function: (address & 0xffff) as u16,
            pin: *pin as u8,
            gsi,
            active_low,
        };
        if routes.iter().any(|r: &Route| {
            r.device == route.device && r.function == route.function && r.pin == route.pin
        }) {
            return Err(BAD_PARAMETER);
        }
        routes.push(route);
    }
    Ok(routes)
}
impl Engine {
    pub fn pci_routes(&self, parent: &str) -> Result<Vec<Route>, Status> {
        parse(
            self.evaluate(&format!("{parent}._PRT"), &[])?,
            |source, index| {
                // Nonzero descriptor selection and inactive/unprogrammed links
                // require allocation/_SRS policy; do not guess a routable IRQ.
                if index != 0 {
                    return Err(SUPPORT);
                }
                let path = if source.starts_with('\\') {
                    String::from(source)
                } else {
                    self.resolve(parent, source)?
                };
                if self.hardware_id(&path).as_deref() != Ok("PNP0C0F") {
                    return Err(SUPPORT);
                }
                let sta = match self.integer(&format!("{path}._STA")) {
                    Ok(v) => v,
                    Err(5) => 0xf,
                    Err(e) => return Err(e),
                };
                if sta & 3 != 3 {
                    return Err(SUPPORT);
                }
                let r = crate::resources::parse(&self.resources(&path, false)?)?;
                if r.irqs.len() != 1
                    || r.irqs[0].numbers.len() != 1
                    || !r.irqs[0].level
                    || r.irqs[0].numbers[0] == 0
                {
                    return Err(SUPPORT);
                }
                Ok((r.irqs[0].numbers[0], r.irqs[0].active_low))
            },
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_and_link_routes() {
        let entries = Value::Package(alloc::vec![
            Value::Package(alloc::vec![
                Value::Integer(0xffff),
                Value::Integer(0),
                Value::Integer(0),
                Value::Integer(16)
            ]),
            Value::Package(alloc::vec![
                Value::Integer(0x1ffff),
                Value::Integer(1),
                Value::String(b"LNKA".to_vec()),
                Value::Integer(0)
            ])
        ]);
        let r = parse(entries, |s, i| {
            assert_eq!((s, i), ("LNKA", 0));
            Ok((11, true))
        })
        .unwrap();
        assert_eq!(r[0].gsi, 16);
        assert_eq!(r[1].gsi, 11);
    }
    #[test]
    fn malformed_routes_rejected() {
        let value = Value::Package(alloc::vec![Value::Package(alloc::vec![
            Value::Integer(0xffff),
            Value::Integer(4),
            Value::Integer(0),
            Value::Integer(16)
        ])]);
        assert!(parse(value, |_, _| Ok((0, true))).is_err());
    }
    #[test]
    fn empty_source_is_direct_and_link_polarity_is_preserved() {
        let entry = |source| {
            Value::Package(alloc::vec![
                Value::Integer(0xffff),
                Value::Integer(0),
                source,
                Value::Integer(22)
            ])
        };
        let direct = parse(
            Value::Package(alloc::vec![entry(Value::String(Vec::new()))]),
            |_, _| panic!("direct route called link"),
        )
        .unwrap();
        assert_eq!((direct[0].gsi, direct[0].active_low), (22, true));
        let linked = parse(
            Value::Package(alloc::vec![entry(Value::Reference(b"GSIA".to_vec()))]),
            |_, _| Ok((22, false)),
        )
        .unwrap();
        assert!(!linked[0].active_low);
    }
    #[test]
    fn bridge_swizzle_and_missing_route_are_not_guessed() {
        let route = Route {
            device: 4,
            function: 0xffff,
            pin: 3,
            gsi: 22,
            active_low: false,
        };
        let mut scopes = alloc::vec![
            Scope {
                number: 0,
                parent: None,
                routes: alloc::vec![route.clone()]
            },
            Scope {
                number: 1,
                parent: Some((0, 4, 0)),
                routes: Vec::new()
            }
        ];
        assert_eq!(resolve(&scopes, 1, 3, 0, 1), Some((22, false)));
        assert_eq!(resolve(&scopes, 1, 2, 0, 1), None);
        assert_eq!(resolve(&scopes, 1, 3, 0, 0), None);
        assert_eq!(resolve(&scopes, 9, 3, 0, 1), None);
        scopes[1].routes.push(route);
        assert_eq!(resolve(&scopes, 1, 3, 0, 1), None);
    }
    #[test]
    fn exact_function_wins_and_cycles_are_bounded() {
        let mut scopes = alloc::vec![Scope {
            number: 0,
            parent: None,
            routes: alloc::vec![
                Route {
                    device: 2,
                    function: 0xffff,
                    pin: 0,
                    gsi: 16,
                    active_low: true
                },
                Route {
                    device: 2,
                    function: 1,
                    pin: 0,
                    gsi: 20,
                    active_low: true
                }
            ]
        }];
        assert_eq!(resolve(&scopes, 0, 2, 1, 1), Some((20, true)));
        scopes[0].routes.clear();
        scopes[0].parent = Some((0, 0, 0));
        assert_eq!(resolve(&scopes, 0, 0, 0, 1), None);
    }
}
