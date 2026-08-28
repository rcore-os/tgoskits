//! Runtime-authenticated PCI endpoint binding and BAR dispatch.

use alloc::{collections::BTreeMap, sync::Arc};

use ax_sync::SpinLock;
use axdevice_base::{
    Device, DeviceContext, DeviceError, DeviceId, DeviceResult, NoopDeviceContext,
};

use super::{PciBarIndex, PciBarRoute, PciBdf, PciRootState};
use crate::{
    AccessWidth, DeviceManagerError, DeviceManagerResult, DeviceNodeId, ServiceCardinality,
    ServiceKey,
};

/// Metadata passed to one endpoint BAR callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciBarAccess {
    route: PciBarRoute,
}

impl PciBarAccess {
    /// Returns the selected function BDF.
    pub const fn bdf(self) -> PciBdf {
        self.route.bdf()
    }
    /// Returns the selected BAR slot.
    pub const fn bar(self) -> PciBarIndex {
        self.route.bar()
    }
    /// Returns the BAR-relative byte offset.
    pub const fn offset(self) -> u64 {
        self.route.offset()
    }
    /// Returns the complete access width.
    pub const fn width(self) -> AccessWidth {
        self.route.width()
    }
}

/// Endpoint-owned behavior reached after authenticated PCI routing.
///
/// # Device context contract
///
/// Callbacks run strictly outside the root lock, after the runtime validated
/// the [`EndpointRouteToken`] and pinned the endpoint with a strong reference.
/// They currently receive an identity-correct but capability-free
/// [`NoopDeviceContext`] carrying the endpoint's final [`DeviceId`]: grants
/// registered through the bundle (guest memory, timers, wake, stop) are not
/// reachable from this path yet. Routing and dispatch ownership stays with
/// [`DeviceRuntime`](crate::DeviceRuntime); the first grant-bearing endpoint
/// must extend that seam in its own design together with a
/// grant-through-BAR-callback regression test. The route token itself never
/// carries or mints capabilities.
pub trait PciFunction: Device {
    /// Reads one complete memory BAR access.
    fn read_bar(&self, access: PciBarAccess, context: &mut dyn DeviceContext) -> DeviceResult<u64>;
    /// Writes one complete memory BAR access.
    fn write_bar(
        &self,
        access: PciBarAccess,
        value: u64,
        context: &mut dyn DeviceContext,
    ) -> DeviceResult;
}

/// Non-capability token identifying one active endpoint binding generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EndpointRouteToken {
    device: DeviceId,
    generation: u64,
}

struct RoutedEndpoint {
    generation: u64,
    function: Arc<dyn PciFunction>,
}

#[derive(Default)]
struct EndpointRouterState {
    next_generation: u64,
    endpoints: BTreeMap<DeviceId, RoutedEndpoint>,
}

struct EndpointRouter {
    state: SpinLock<EndpointRouterState>,
}

impl EndpointRouter {
    fn new() -> Self {
        Self {
            state: SpinLock::new(EndpointRouterState::default()),
        }
    }

    fn activate(
        &self,
        device: DeviceId,
        function: Arc<dyn PciFunction>,
    ) -> DeviceManagerResult<EndpointRouteToken> {
        let mut state = self.state.lock_irqsave();
        if state.endpoints.contains_key(&device) {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "bind PCI endpoint route",
                detail: alloc::format!(
                    "device {} already has an active PCI route",
                    device.as_u32()
                ),
            });
        }
        state.next_generation = state.next_generation.checked_add(1).ok_or_else(|| {
            DeviceManagerError::InvalidState {
                operation: "bind PCI endpoint route",
                detail: "PCI binding generation is exhausted".into(),
            }
        })?;
        let token = EndpointRouteToken {
            device,
            generation: state.next_generation,
        };
        state.endpoints.insert(
            device,
            RoutedEndpoint {
                generation: token.generation,
                function,
            },
        );
        Ok(token)
    }

    fn invalidate(&self, token: EndpointRouteToken) -> Option<Arc<dyn PciFunction>> {
        let mut state = self.state.lock_irqsave();
        if state
            .endpoints
            .get(&token.device)
            .is_some_and(|entry| entry.generation == token.generation)
        {
            return state
                .endpoints
                .remove(&token.device)
                .map(|entry| entry.function);
        }
        None
    }

    fn endpoint(&self, token: EndpointRouteToken) -> DeviceResult<Arc<dyn PciFunction>> {
        let state = self.state.lock_irqsave();
        state
            .endpoints
            .get(&token.device)
            .filter(|entry| entry.generation == token.generation)
            .map(|entry| entry.function.clone())
            .ok_or_else(|| DeviceError::InvalidState {
                operation: "dispatch PCI endpoint route",
                detail: "PCI endpoint route token is stale".into(),
            })
    }
}

/// Host-owned root binding published as a typed bundle service.
pub struct PciRootBinding {
    host: DeviceNodeId,
    root: Arc<PciRootState>,
    router: Arc<EndpointRouter>,
}

impl PciRootBinding {
    /// Creates a binding service for one resolved host root.
    pub fn new(host: DeviceNodeId, root: Arc<PciRootState>) -> Self {
        Self {
            host,
            root,
            router: Arc::new(EndpointRouter::new()),
        }
    }

    /// Returns the host graph identity publishing this service.
    pub const fn host(&self) -> &DeviceNodeId {
        &self.host
    }

    pub(crate) fn matches_topology(&self, topology: &Arc<super::ResolvedPciTopology>) -> bool {
        Arc::ptr_eq(self.root.topology_arc(), topology)
    }

    pub(crate) fn bind(
        self: &Arc<Self>,
        function_id: &DeviceNodeId,
        device: DeviceId,
        function: Arc<dyn PciFunction>,
    ) -> DeviceManagerResult<PciBindingLease> {
        let token = self.router.activate(device, function)?;
        if let Err(error) = self.root.bind_endpoint(function_id, token) {
            drop(self.router.invalidate(token));
            return Err(error.into());
        }
        Ok(PciBindingLease {
            binding: self.clone(),
            token,
        })
    }

    /// Dispatches a BAR read after root lookup and token validation.
    pub fn read_bar(&self, address: u64, width: AccessWidth) -> DeviceResult<u64> {
        let (token, route) = self
            .root
            .resolve_bound_bar(address, width)
            .ok_or(DeviceError::NotFound)?;
        let endpoint = self.router.endpoint(token)?;
        let mut context = NoopDeviceContext::new(token.device);
        endpoint.read_bar(PciBarAccess { route }, &mut context)
    }

    /// Dispatches a BAR write after root lookup and token validation.
    pub fn write_bar(&self, address: u64, width: AccessWidth, value: u64) -> DeviceResult {
        let (token, route) = self
            .root
            .resolve_bound_bar(address, width)
            .ok_or(DeviceError::NotFound)?;
        let endpoint = self.router.endpoint(token)?;
        let mut context = NoopDeviceContext::new(token.device);
        endpoint.write_bar(PciBarAccess { route }, value, &mut context)
    }
}

/// Typed service key published only by a PCI host bundle.
///
/// Bindings stay enumerable through `DeviceRuntime::services()` for
/// diagnostics and host-side verification; endpoint models never receive a
/// `DeviceRuntime`, so route resolution remains dependency-scoped.
pub struct PciRootBindingKey;

impl ServiceKey for PciRootBindingKey {
    type Service = PciRootBinding;
    const NAME: &'static str = "pci-root-binding";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Multiple;
}

pub(crate) struct PciBindingLease {
    binding: Arc<PciRootBinding>,
    token: EndpointRouteToken,
}

impl Drop for PciBindingLease {
    fn drop(&mut self) {
        // Teardown order from the design (§7.1): invalidate the binding
        // generation first so new validations fail, then withdraw the root
        // route, and only then release the strong endpoint reference kept
        // since dispatch validation - in-flight callbacks finish safely.
        let endpoint = self.binding.router.invalidate(self.token);
        self.binding.root.unbind_endpoint(self.token);
        drop(endpoint);
    }
}

#[cfg(test)]
mod tests {
    use axdevice_base::{DeviceAccess, Resource};

    use super::*;
    use crate::PciError;

    struct StubFunction;

    impl Device for StubFunction {
        fn name(&self) -> &str {
            "stub-pci-function"
        }
        fn resources(&self) -> &[Resource] {
            &[]
        }
        fn read(
            &self,
            _access: &DeviceAccess,
            _context: &mut dyn DeviceContext,
        ) -> DeviceResult<u64> {
            Err(DeviceError::NotFound)
        }
        fn write(
            &self,
            _access: &DeviceAccess,
            _value: u64,
            _context: &mut dyn DeviceContext,
        ) -> DeviceResult {
            Ok(())
        }
    }

    impl PciFunction for StubFunction {
        fn read_bar(
            &self,
            _access: PciBarAccess,
            _context: &mut dyn DeviceContext,
        ) -> DeviceResult<u64> {
            Ok(0)
        }
        fn write_bar(
            &self,
            _access: PciBarAccess,
            _value: u64,
            _context: &mut dyn DeviceContext,
        ) -> DeviceResult {
            Ok(())
        }
    }

    fn router() -> EndpointRouter {
        EndpointRouter {
            state: SpinLock::new(EndpointRouterState::default()),
        }
    }

    #[test]
    fn rebind_mints_a_new_generation_and_rejects_stale_tokens() {
        let router = router();
        let function: Arc<dyn PciFunction> = Arc::new(StubFunction);
        let device = DeviceId::new(7);

        let first = router.activate(device, Arc::clone(&function)).unwrap();
        assert_eq!(first.generation, 1);
        assert!(router.endpoint(first).is_ok());

        let removed = router.invalidate(first).unwrap();
        assert!(Arc::ptr_eq(&removed, &function));
        let second = router.activate(device, Arc::clone(&function)).unwrap();
        assert_eq!(second.generation, 2);

        // The old generation can never dispatch again, before or after the
        // new binding exists.
        assert!(matches!(
            router.endpoint(first),
            Err(DeviceError::InvalidState { .. })
        ));
        drop(router.endpoint(second).unwrap());
    }

    #[test]
    fn invalidate_returns_none_for_unknown_or_stale_tokens() {
        let router = router();
        let function: Arc<dyn PciFunction> = Arc::new(StubFunction);
        let device = DeviceId::new(3);

        let token = router.activate(device, Arc::clone(&function)).unwrap();
        let forged = EndpointRouteToken {
            device,
            generation: token.generation + 1,
        };
        assert!(router.invalidate(forged).is_none());
        assert!(router.endpoint(token).is_ok());
        assert_eq!(
            router.invalidate(token).map(|arc| Arc::strong_count(&arc)),
            Some(2)
        );
        assert!(router.invalidate(token).is_none());
    }

    #[test]
    fn root_rejects_a_second_binding_for_the_same_function() {
        use crate::{PciClass, PciEndpointIdentity, PciFunctionSpec, PciTopologyBuilder};

        let mut builder = PciTopologyBuilder::new();
        builder
            .add_function(PciFunctionSpec::new(
                DeviceNodeId::new("endpoint").unwrap(),
                PciEndpointIdentity::new(0x1af4, 0x1110, PciClass::new(0xff, 0, 0)),
            ))
            .unwrap();
        let topology = Arc::new(builder.resolve(0xc000_0000..0xc100_0000).unwrap());
        let root = PciRootState::new(Arc::clone(&topology));
        let function_id = DeviceNodeId::new("endpoint").unwrap();

        let router = router();
        let function: Arc<dyn PciFunction> = Arc::new(StubFunction);
        let first = router
            .activate(DeviceId::new(1), Arc::clone(&function))
            .unwrap();
        root.bind_endpoint(&function_id, first).unwrap();
        assert!(matches!(
            root.bind_endpoint(&function_id, first),
            Err(PciError::FunctionAlreadyBound { .. })
        ));

        // Unbind invalidates the route; the same token never revives.
        drop(router.invalidate(first));
        root.unbind_endpoint(first);
        assert_eq!(root.resolve_bound_bar(0xc000_0000, AccessWidth::Byte), None);
        let second = router
            .activate(DeviceId::new(1), Arc::clone(&function))
            .unwrap();
        root.bind_endpoint(&function_id, second).unwrap();
    }
}
