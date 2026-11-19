//! The `curios:open` key mapping and its serverbound open request.
//!
//! Horizon has no server-side key hook: the client registers the mapping and
//! sends a one-byte payload, and the server opens the authoritative menu. The
//! key and label follow the Java reference (`CuriosKeyMappings`, default G).
//!
//! Adapted from TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright 2018-2024
//! C4); see COPYING and COPYING.LESSER.

use std::sync::Arc;

use mod_api::{
    ClientHost, HandlerThread, IPayloadContext, IPayloadHandler, KeyConflictContext,
    KeyRegistration, PayloadBytes, PayloadDeclaration, PayloadDirection, PayloadHandleError,
    PayloadId, PayloadProtocol,
};

/// Mapping id registered with `Registrar::register_key_mapping`.
pub const OPEN_KEY_ID: &str = "open";
/// Vanilla key `G`, matching `InputConstants.KEY_G` in the Java reference.
pub const OPEN_KEY_DEFAULT: &str = "key.keyboard.g";
/// Label translated through the shipped lang tables.
pub const OPEN_KEY_LABEL: &str = "key.curios.open.desc";

/// Channel carrying the open request.
pub const OPEN_REQUEST_ID: &str = "curios:open_request";
pub const OPEN_REQUEST_VERSION: &str = "1";

/// Channel carrying a render-toggle request for one visible menu slot.
pub const RENDER_REQUEST_ID: &str = "curios:toggle_render";
pub const RENDER_REQUEST_VERSION: &str = "1";

/// Payload contents: a single action byte. Kept explicit so a truncated or
/// malformed packet is refused instead of being treated as an open.
const OPEN_ACTION: u8 = 0b0000_0001;

pub fn key_registration() -> KeyRegistration {
    KeyRegistration::new(OPEN_KEY_ID, OPEN_KEY_DEFAULT)
        .label(OPEN_KEY_LABEL)
        .context(KeyConflictContext::Universal)
}

pub fn key_payload_id() -> PayloadId {
    PayloadId(OPEN_REQUEST_ID.to_owned())
}

pub fn key_payload_declaration() -> PayloadDeclaration {
    PayloadDeclaration::new(
        key_payload_id(),
        OPEN_REQUEST_VERSION.to_owned(),
        PayloadProtocol::Play,
        PayloadDirection::ToServer,
        false,
        HandlerThread::Main,
    )
}

/// Encodes one key-down as a request to open the Curios menu.
pub fn encode_open_request() -> PayloadBytes {
    PayloadBytes(vec![OPEN_ACTION])
}

/// Decodes the request strictly: exactly one byte with the action bit set.
pub fn decode_open_request(bytes: &PayloadBytes) -> Result<(), PayloadHandleError> {
    match bytes.0.as_slice() {
        [flag] if flag & OPEN_ACTION != 0 => Ok(()),
        _ => Err(PayloadHandleError::Failed(
            "malformed curios open request".to_owned(),
        )),
    }
}

/// Client hook: send the request once per key-down, never on release and
/// never for another mod's mapping.
pub fn on_mapping_press(host: &mut dyn ClientHost, mapping: &str, pressed: bool) -> bool {
    if mapping != OPEN_KEY_ID || !pressed {
        return false;
    }
    host.send_payload_to_server(&key_payload_id(), encode_open_request())
        .is_ok()
}

/// Server payload handler; register it with `Registrar::add_payload`.
pub struct OpenRequestHandler;

impl IPayloadHandler for OpenRequestHandler {
    fn handle(
        &self,
        payload: PayloadBytes,
        context: &mut dyn IPayloadContext,
    ) -> Result<(), PayloadHandleError> {
        decode_open_request(&payload)?;
        let player = context
            .player()
            .map_err(|error| PayloadHandleError::Failed(format!("{error:?}")))?;
        let host = context
            .server_host()
            .map_err(|error| PayloadHandleError::Failed(format!("{error:?}")))?;
        if !crate::menu::open(host, player.uuid()) {
            eprintln!("[curios] could not open the Curios menu for {}", player.uuid());
        }
        Ok(())
    }
}

pub fn key_press_handler() -> Arc<dyn IPayloadHandler> {
    Arc::new(OpenRequestHandler)
}

/// Encodes a slot index as two little-endian bytes. The index is the visible
/// menu slot, resolved to a curio cell on the server.
pub fn encode_render_toggle(slot: usize) -> PayloadBytes {
    let index = (slot as u16).to_le_bytes();
    PayloadBytes(vec![OPEN_ACTION, index[0], index[1]])
}

pub fn decode_render_toggle(bytes: &PayloadBytes) -> Result<usize, PayloadHandleError> {
    match bytes.0.as_slice() {
        [flag, low, high] if flag & OPEN_ACTION != 0 => {
            Ok(usize::from(u16::from_le_bytes([*low, *high])))
        }
        _ => Err(PayloadHandleError::Failed(
            "malformed curios render toggle".to_owned(),
        )),
    }
}

pub fn render_payload_id() -> PayloadId {
    PayloadId(RENDER_REQUEST_ID.to_owned())
}

pub fn render_payload_declaration() -> PayloadDeclaration {
    PayloadDeclaration::new(
        render_payload_id(),
        RENDER_REQUEST_VERSION.to_owned(),
        PayloadProtocol::Play,
        PayloadDirection::ToServer,
        false,
        HandlerThread::Main,
    )
}

/// Client-side request. Requires a mutable client host, unlike the screen
/// hooks, so `lib.rs` should call this from a key-mapping hook.
pub fn send_render_toggle(host: &mut dyn ClientHost, slot: usize) -> bool {
    host.send_payload_to_server(&render_payload_id(), encode_render_toggle(slot))
        .is_ok()
}

/// Server handler for the render toggle.
pub struct RenderToggleHandler;

impl IPayloadHandler for RenderToggleHandler {
    fn handle(
        &self,
        payload: PayloadBytes,
        context: &mut dyn IPayloadContext,
    ) -> Result<(), PayloadHandleError> {
        let slot = decode_render_toggle(&payload)?;
        let player = context
            .player()
            .map_err(|error| PayloadHandleError::Failed(format!("{error:?}")))?;
        let host = context
            .server_host()
            .map_err(|error| PayloadHandleError::Failed(format!("{error:?}")))?;
        if !crate::menu::toggle_render(host, player.uuid(), slot) {
            eprintln!("[curios] render toggle refused for slot {slot}");
        }
        Ok(())
    }
}

pub fn render_handler() -> Arc<dyn IPayloadHandler> {
    Arc::new(RenderToggleHandler)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mapping_matches_the_reference() {
        assert_eq!(OPEN_KEY_ID, "open");
        assert_eq!(OPEN_KEY_DEFAULT, "key.keyboard.g");
        assert_eq!(OPEN_KEY_LABEL, "key.curios.open.desc");
        let registration = key_registration();
        assert_eq!(registration.id, OPEN_KEY_ID);
    }

    #[test]
    fn the_payload_channel_is_serverbound_play() {
        let declaration = key_payload_declaration();
        assert_eq!(declaration.id, key_payload_id());
        assert_eq!(declaration.direction, PayloadDirection::ToServer);
        assert_eq!(declaration.protocol, PayloadProtocol::Play);
    }

    #[test]
    fn open_request_is_strictly_decoded() {
        assert_eq!(decode_open_request(&encode_open_request()), Ok(()));
        assert!(decode_open_request(&PayloadBytes(Vec::new())).is_err());
        assert!(decode_open_request(&PayloadBytes(vec![0b0000_0010])).is_err());
        assert!(decode_open_request(&PayloadBytes(vec![OPEN_ACTION, 0])).is_err());
    }

    #[test]
    fn render_toggle_carries_the_slot_index() {
        for slot in [0usize, 1, 47, 300] {
            assert_eq!(decode_render_toggle(&encode_render_toggle(slot)), Ok(slot));
        }
        assert!(decode_render_toggle(&encode_open_request()).is_err());
        assert!(decode_render_toggle(&PayloadBytes(vec![OPEN_ACTION, 1])).is_err());
        let declaration = render_payload_declaration();
        assert_eq!(declaration.id, render_payload_id());
        assert_eq!(declaration.direction, PayloadDirection::ToServer);
    }
}
