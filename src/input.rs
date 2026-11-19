//! The `curios:open` key mapping and its serverbound open request.
//!
//! Horizon has no server-side key hook: the client registers the mapping and
//! sends a one-byte payload, and the server opens the authoritative menu. The
//! key and label follow the Java reference (`CuriosKeyMappings`, default G).
//!
//! Adapted from TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright 2018-2024
//! C4); see COPYING and COPYING.LESSER.

use std::sync::{Arc, Mutex};

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

/// Serverbound channel carrying the screen snapshot: page, cosmetic view and
/// each displayed stack with its active/render flags. The attachment is not
/// synced to clients at this revision, so the screen needs an explicit view.
pub const SCREEN_STATE_ID: &str = "curios:screen_state";
pub const SCREEN_STATE_VERSION: &str = "1";

/// One cell of the wire snapshot. `flags`: bit 0 cosmetic, bit 1 active,
/// bit 2 render, bit 3 has stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenCellWire {
    pub index: u16,
    pub flags: u8,
    pub nbt: Vec<u8>,
}

pub const CELL_COSMETIC: u8 = 0b0000_0001;
pub const CELL_ACTIVE: u8 = 0b0000_0010;
pub const CELL_RENDER: u8 = 0b0000_0100;
pub const CELL_HAS_STACK: u8 = 0b0000_1000;

/// Channel carrying the remaining client menu requests (Java `client_page`,
/// `toggle_cosmetics`, `destroy`, `open_vanilla`).
pub const MENU_REQUEST_ID: &str = "curios:menu_request";
pub const MENU_REQUEST_VERSION: &str = "1";

/// Action byte of the menu-request channel.
pub const ACTION_PAGE_NEXT: u8 = 1;
pub const ACTION_PAGE_PREV: u8 = 2;
pub const ACTION_TOGGLE_COSMETICS: u8 = 3;
pub const ACTION_DESTROY: u8 = 4;
pub const ACTION_OPEN_VANILLA: u8 = 5;

/// A client-side button press that a screen hook observed but could not send:
/// those hooks hold an immutable `ClientHost`. `flush_pending_actions` drains
/// this queue from `key_mapping_pre`, which does have a mutable host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingAction {
    PageNext,
    PagePrev,
    ToggleCosmetics,
    ToggleRender(usize),
    Destroy,
    OpenVanilla,
}

static PENDING: Mutex<Vec<PendingAction>> = Mutex::new(Vec::new());

pub fn queue(action: PendingAction) {
    let mut guard = PENDING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.push(action);
}

/// Sends every queued action and returns the number delivered. A send failure
/// is reported instead of silently dropping the request.
pub fn flush_pending_actions(host: &mut dyn ClientHost) -> usize {
    let actions: Vec<PendingAction> = std::mem::take(
        &mut *PENDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    let mut sent = 0usize;
    for action in actions {
        let ok = match action {
            PendingAction::PageNext => send_page(host, true),
            PendingAction::PagePrev => send_page(host, false),
            PendingAction::ToggleCosmetics => send_toggle_cosmetics(host),
            PendingAction::ToggleRender(slot) => send_render_toggle(host, slot),
            PendingAction::Destroy => send_destroy(host),
            PendingAction::OpenVanilla => send_open_vanilla(host),
        };
        if ok {
            sent += 1;
        } else {
            eprintln!("[curios] queued menu action {action:?} could not be sent");
        }
    }
    sent
}

/// Encodes a screen snapshot. Layout: page, cosmetic, cell count, then for
/// each cell `(index u16 LE, flags u8, nbt length u16 LE, nbt bytes)`.
pub fn encode_screen_state(page: u8, cosmetic: bool, cells: &[ScreenCellWire]) -> PayloadBytes {
    let mut bytes = vec![page, u8::from(cosmetic), cells.len() as u8];
    for cell in cells {
        bytes.extend_from_slice(&cell.index.to_le_bytes());
        bytes.push(cell.flags);
        let len = cell.nbt.len().min(u16::MAX as usize) as u16;
        bytes.extend_from_slice(&len.to_le_bytes());
        bytes.extend_from_slice(&cell.nbt[..usize::from(len)]);
    }
    PayloadBytes(bytes)
}

/// Decodes a snapshot strictly: every declared cell must be complete, and
/// no trailing bytes are accepted.
pub fn decode_screen_state(
    bytes: &PayloadBytes,
) -> Result<(u8, bool, Vec<ScreenCellWire>), PayloadHandleError> {
    let invalid = || PayloadHandleError::Failed("malformed curios screen state".to_owned());
    let mut iter = bytes.0.iter().copied();
    let page = iter.next().ok_or_else(invalid)?;
    let cosmetic = match iter.next().ok_or_else(invalid)? {
        0 => false,
        1 => true,
        _ => return Err(invalid()),
    };
    let count = usize::from(iter.next().ok_or_else(invalid)?);
    let mut cells = Vec::with_capacity(count);
    for _ in 0..count {
        let low = iter.next().ok_or_else(invalid)?;
        let high = iter.next().ok_or_else(invalid)?;
        let flags = iter.next().ok_or_else(invalid)?;
        let nlo = iter.next().ok_or_else(invalid)?;
        let nhi = iter.next().ok_or_else(invalid)?;
        let len = usize::from(u16::from_le_bytes([nlo, nhi]));
        let mut nbt = Vec::with_capacity(len);
        for _ in 0..len {
            nbt.push(iter.next().ok_or_else(invalid)?);
        }
        cells.push(ScreenCellWire {
            index: u16::from_le_bytes([low, high]),
            flags,
            nbt,
        });
    }
    if iter.next().is_some() {
        return Err(invalid());
    }
    Ok((page, cosmetic, cells))
}

pub fn screen_state_payload_id() -> PayloadId {
    PayloadId(SCREEN_STATE_ID.to_owned())
}

pub fn screen_state_payload_declaration() -> PayloadDeclaration {
    PayloadDeclaration::new(
        screen_state_payload_id(),
        SCREEN_STATE_VERSION.to_owned(),
        PayloadProtocol::Play,
        PayloadDirection::ToClient,
        false,
        HandlerThread::Main,
    )
}

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

/// Encodes one menu action. The payload is action-only: the server keeps the
/// window id of the menu it opened, so a stale client cannot target another
/// player's menu.
pub fn encode_menu_request(action: u8) -> PayloadBytes {
    PayloadBytes(vec![action])
}

pub fn decode_menu_request(bytes: &PayloadBytes) -> Result<u8, PayloadHandleError> {
    match bytes.0.as_slice() {
        [action] if matches!(
            *action,
            ACTION_PAGE_NEXT
                | ACTION_PAGE_PREV
                | ACTION_TOGGLE_COSMETICS
                | ACTION_DESTROY
                | ACTION_OPEN_VANILLA
        ) => Ok(*action),
        _ => Err(PayloadHandleError::Failed(
            "malformed curios menu request".to_owned(),
        )),
    }
}

pub fn menu_payload_id() -> PayloadId {
    PayloadId(MENU_REQUEST_ID.to_owned())
}

pub fn menu_payload_declaration() -> PayloadDeclaration {
    PayloadDeclaration::new(
        menu_payload_id(),
        MENU_REQUEST_VERSION.to_owned(),
        PayloadProtocol::Play,
        PayloadDirection::ToServer,
        false,
        HandlerThread::Main,
    )
}

/// Client-side request helpers. Every one needs a mutable client host, which
/// `key_mapping_pre` provides; the immutable screen hooks only update the
/// local view and must not pretend to have changed the server.
pub fn send_page(host: &mut dyn ClientHost, next: bool) -> bool {
    let action = if next { ACTION_PAGE_NEXT } else { ACTION_PAGE_PREV };
    host.send_payload_to_server(&menu_payload_id(), encode_menu_request(action))
        .is_ok()
}

pub fn send_toggle_cosmetics(host: &mut dyn ClientHost) -> bool {
    host.send_payload_to_server(
        &menu_payload_id(),
        encode_menu_request(ACTION_TOGGLE_COSMETICS),
    )
    .is_ok()
}

pub fn send_destroy(host: &mut dyn ClientHost) -> bool {
    host.send_payload_to_server(&menu_payload_id(), encode_menu_request(ACTION_DESTROY))
        .is_ok()
}

pub fn send_open_vanilla(host: &mut dyn ClientHost) -> bool {
    host.send_payload_to_server(
        &menu_payload_id(),
        encode_menu_request(ACTION_OPEN_VANILLA),
    )
    .is_ok()
}

/// Server handler, registered by `lib.rs` in `Hooks::register`.
pub struct MenuRequestHandler;

impl IPayloadHandler for MenuRequestHandler {
    fn handle(
        &self,
        payload: PayloadBytes,
        context: &mut dyn IPayloadContext,
    ) -> Result<(), PayloadHandleError> {
        let action = decode_menu_request(&payload)?;
        let player = context
            .player()
            .map_err(|error| PayloadHandleError::Failed(format!("{error:?}")))?;
        let host = context
            .server_host()
            .map_err(|error| PayloadHandleError::Failed(format!("{error:?}")))?;
        let uuid = player.uuid();
        let accepted = match action {
            ACTION_PAGE_NEXT => crate::menu::client_page(host, uuid, true),
            ACTION_PAGE_PREV => crate::menu::client_page(host, uuid, false),
            ACTION_TOGGLE_COSMETICS => crate::menu::toggle_cosmetics(host, uuid),
            ACTION_DESTROY => crate::menu::destroy(host, uuid),
            ACTION_OPEN_VANILLA => crate::menu::open_vanilla(host, uuid),
            _ => false,
        };
        if !accepted {
            eprintln!("[curios] menu request {action} refused for player {uuid}");
        }
        Ok(())
    }
}

pub fn menu_handler() -> Arc<dyn IPayloadHandler> {
    Arc::new(MenuRequestHandler)
}

/// Applies a server `grabbed_item` response on the client. The pinned API can
/// only add the stack to the inventory; there is no cursor write from a
/// client hook, so this is deliberately described as an inventory return.
pub fn apply_grabbed_item(host: &mut dyn ClientHost, stack: &mod_api::ItemStack) -> bool {
    if stack.count() == 0 {
        return false;
    }
    host.give(stack.item(), stack.count()).is_ok()
}

/// Client handler for the screen snapshot.
pub struct ScreenStateHandler;

impl IPayloadHandler for ScreenStateHandler {
    fn handle(
        &self,
        payload: PayloadBytes,
        context: &mut dyn IPayloadContext,
    ) -> Result<(), PayloadHandleError> {
        let (page, cosmetic, cells) = decode_screen_state(&payload)?;
        let uuid = context
            .player()
            .map_err(|error| PayloadHandleError::Failed(format!("{error:?}")))?
            .uuid();
        let host = context
            .client_host()
            .map_err(|error| PayloadHandleError::Failed(format!("{error:?}")))?;
        crate::menu::apply_screen_state(uuid, host, page, cosmetic, &cells);
        Ok(())
    }
}

pub fn screen_state_handler() -> Arc<dyn IPayloadHandler> {
    Arc::new(ScreenStateHandler)
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
    fn screen_actions_queue_in_order() {
        let mut guard = PENDING.lock().unwrap();
        guard.clear();
        drop(guard);
        queue(PendingAction::PageNext);
        queue(PendingAction::ToggleRender(5));
        queue(PendingAction::ToggleCosmetics);
        let mut guard = PENDING.lock().unwrap();
        assert_eq!(
            *guard,
            vec![
                PendingAction::PageNext,
                PendingAction::ToggleRender(5),
                PendingAction::ToggleCosmetics
            ]
        );
        guard.clear();
    }

    #[test]
    fn screen_state_round_trips_and_rejects_truncation() {
        let cells = vec![
            ScreenCellWire { index: 7, flags: CELL_ACTIVE | CELL_HAS_STACK, nbt: vec![1, 2, 3] },
            ScreenCellWire { index: 9, flags: CELL_COSMETIC | CELL_RENDER, nbt: vec![] },
        ];
        let encoded = encode_screen_state(2, true, &cells);
        assert_eq!(decode_screen_state(&encoded), Ok((2, true, cells.clone())));
        let mut truncated = encode_screen_state(0, false, &[cells[0].clone()]).0;
        truncated.pop();
        assert!(decode_screen_state(&PayloadBytes(truncated)).is_err());
        assert!(decode_screen_state(&PayloadBytes(vec![0, 2, 0])).is_err());
        let declaration = screen_state_payload_declaration();
        assert_eq!(declaration.id, screen_state_payload_id());
        assert_eq!(declaration.direction, PayloadDirection::ToClient);
    }

    #[test]
    fn menu_requests_are_strictly_decoded() {
        for action in [
            ACTION_PAGE_NEXT,
            ACTION_PAGE_PREV,
            ACTION_TOGGLE_COSMETICS,
            ACTION_DESTROY,
            ACTION_OPEN_VANILLA,
        ] {
            assert_eq!(decode_menu_request(&encode_menu_request(action)), Ok(action));
        }
        assert!(decode_menu_request(&PayloadBytes(Vec::new())).is_err());
        assert!(decode_menu_request(&PayloadBytes(vec![0])).is_err());
        assert!(decode_menu_request(&PayloadBytes(vec![ACTION_DESTROY, 0])).is_err());
        let declaration = menu_payload_declaration();
        assert_eq!(declaration.id, menu_payload_id());
        assert_eq!(declaration.direction, PayloadDirection::ToServer);
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
