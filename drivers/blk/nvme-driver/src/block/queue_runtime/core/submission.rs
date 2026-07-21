use super::*;

impl NvmeQueueCore {
    /// Publishes request identity and ownership before ringing the SQ doorbell.
    ///
    /// A returned [`UnacceptedRequest`] proves that no descriptor or doorbell
    /// became hardware-visible. Once this method returns accepted, every
    /// terminal result must arrive through CQ evidence or proof-gated recovery.
    pub(in crate::block) fn submit_owned(
        &self,
        namespace: Namespace,
        max_transfer_bytes: Option<usize>,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<AcceptedInterruptRequest, UnacceptedRequest> {
        if let Err(error) =
            validate_owned_request(self.queue_info_for(namespace, max_transfer_bytes), &request)
        {
            return Err(UnacceptedRequest::new(id, error, request));
        }
        self.submit_prepared(namespace, id, request)
            .map(|()| AcceptedInterruptRequest::new(id))
            .map_err(SubmitError::into_unaccepted)
    }

    fn submit_prepared(
        &self,
        namespace: Namespace,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<(), SubmitError> {
        let (mut request, mut prepared) = prepare_request_dma(id, request)?;
        let Some(mut state) = self.try_claim_state() else {
            request = restore_prepared_dma(request, prepared.take());
            return Err(SubmitError::new(id, BlkError::Retry, request));
        };
        let identity = match state.alloc_identity() {
            Ok(identity) => identity,
            Err(error) => {
                request = restore_prepared_dma(request, prepared.take());
                return Err(SubmitError::new(id, error.into_block_error(), request));
            }
        };
        let (command, prp_list) = match state.build_command(
            namespace,
            self.page_size,
            identity,
            &request,
            prepared.as_ref(),
        ) {
            Ok(command) => command,
            Err(error) => {
                state.release_unaccepted(identity);
                request = restore_prepared_dma(request, prepared.take());
                return Err(SubmitError::new(id, error, request));
            }
        };
        let dma = prepared
            .take()
            // SAFETY: accepted ownership is installed before the SQ doorbell.
            // CQ completion or proof-gated recovery returns DMA ownership only
            // after the controller can no longer access this request.
            .map(|prepared| unsafe { prepared.into_in_flight() });
        state.accept(identity, AcceptedRequest { id, request, dma }, prp_list);
        drop(state);

        // Runtime identity and request ownership are Release-visible before
        // hardware can assert the source consumed by the maintenance owner.
        self.submit_command(command);
        Ok(())
    }
}
