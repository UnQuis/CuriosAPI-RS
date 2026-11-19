//! Networking for the Curios accessory slots.
//!
//! Sixteen typed Horizon payloads mirror the Java reference's `SPacket*`/`CPacket*`
//! set (see `docs/research/curios-behavior.md`, §7). Every request from a client is
//! answered on the server; inventory mutations remain server-authoritative.
//!
//! The item NBT escape is carried as [`NetTag`], a lossless-for-our-data wire form
//! of Horizon's NBT `Tag`. The codec is pure — no `Host` is needed to encode or
//! decode — so the same bytes work natively and in a wasm guest.
//!
//! Adapted from the packet layout of TheIllusiveC4/Curios (LGPL-3.0-or-later,
//! copyright 2018-2024 C4); see COPYING and COPYING.LESSER.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use mod_api::{
    AttachmentError, Compound, IPayloadContext, ItemStack, PayloadId, PayloadSendError, Registrar,
    ServerHost, Tag, ToClient,
};

use crate::inventory::{CurioInventory, ItemSnapshot, SlotStacks};
use crate::slots::SlotType;

/// The single exact schema version of every channel in this module.
pub const VERSION: &str = "1";

/// Equipment (functional) slot in `sync_stack`.
pub const HANDLER_EQUIPMENT: u8 = 0;
/// Cosmetic slot in `sync_stack`.
pub const HANDLER_COSMETIC: u8 = 1;

// ---------------------------------------------------------------------------
// Wire form of NBT.
// ---------------------------------------------------------------------------

/// Why an untrusted wire value is not a usable NBT tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecError {
    /// A stack must be a compound; another tag shape was sent.
    ExpectedCompound,
    /// A value's shape was outside what this codec accepts.
    InvalidValue,
}

/// One named field of a compound, as a payload value. A struct (rather than a
/// tuple) is used because `PayloadCodec` is implemented for records, not tuples.
#[derive(Clone, Debug, PartialEq, mod_api::PayloadCodec)]
pub struct NetEntry {
    pub key: String,
    pub value: NetTag,
}

/// A pure, host-independent copy of an NBT tag.
///
/// Float and double variants keep their IEEE bit pattern, as Horizon's network
/// NBT does; strings are UTF-8 with a lossy conversion for the rare unpaired
/// UTF-16 surrogate. Item ids, slot ids and data-component keys are ASCII.
#[derive(Clone, Debug, PartialEq, mod_api::PayloadCodec)]
pub enum NetTag {
    End,
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(i32),
    Double(i64),
    ByteArray(Vec<u8>),
    String(String),
    List(Vec<NetTag>),
    Compound(Vec<NetEntry>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

/// Copies a saved tag into its wire form.
pub fn tag_to_net(tag: &Tag) -> NetTag {
    match tag {
        Tag::End => NetTag::End,
        Tag::Byte(value) => NetTag::Byte(*value),
        Tag::Short(value) => NetTag::Short(*value),
        Tag::Int(value) => NetTag::Int(*value),
        Tag::Long(value) => NetTag::Long(*value),
        Tag::Float(bits) => NetTag::Float(*bits as i32),
        Tag::Double(bits) => NetTag::Double(*bits as i64),
        Tag::ByteArray(values) => NetTag::ByteArray(values.clone()),
        Tag::String(text) => NetTag::String(text.to_string_lossy()),
        Tag::List(values) => NetTag::List(values.iter().map(tag_to_net).collect()),
        Tag::Compound(values) => NetTag::Compound(
            values
                .iter()
                .map(|(key, value)| NetEntry {
                    key: key.to_string_lossy(),
                    value: tag_to_net(value),
                })
                .collect(),
        ),
        Tag::IntArray(values) => NetTag::IntArray(values.clone()),
        Tag::LongArray(values) => NetTag::LongArray(values.clone()),
    }
}

/// Rebuilds a tag from a wire value. Shape mismatches are refused explicitly.
pub fn net_to_tag(value: &NetTag) -> Result<Tag, CodecError> {
    Ok(match value {
        NetTag::End => Tag::End,
        NetTag::Byte(value) => Tag::Byte(*value),
        NetTag::Short(value) => Tag::Short(*value),
        NetTag::Int(value) => Tag::Int(*value),
        NetTag::Long(value) => Tag::Long(*value),
        NetTag::Float(bits) => Tag::Float(*bits as u32),
        NetTag::Double(bits) => Tag::Double(*bits as u64),
        NetTag::ByteArray(values) => Tag::ByteArray(values.clone()),
        NetTag::String(text) => Tag::String(text.as_str().into()),
        NetTag::List(values) => Tag::List(
            values
                .iter()
                .map(net_to_tag)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        NetTag::Compound(entries) => {
            let mut compound = Compound::new();
            for entry in entries {
                compound.insert(entry.key.as_str().into(), net_to_tag(&entry.value)?);
            }
            Tag::Compound(compound)
        }
        NetTag::IntArray(values) => Tag::IntArray(values.clone()),
        NetTag::LongArray(values) => Tag::LongArray(values.clone()),
    })
}

/// Converts a stored item's NBT escape into a wire stack.
pub fn stack_to_net(item: &ItemSnapshot) -> NetTag {
    tag_to_net(&Tag::Compound(item.as_nbt().clone()))
}

/// Optional wire stack from an optional stored item.
pub fn optional_stack_to_net(item: Option<&ItemSnapshot>) -> Option<NetTag> {
    item.map(stack_to_net)
}

/// Restores a stack through the host. Unknown ids and malformed patches are refused.
pub fn net_to_stack(host: &dyn mod_api::Host, value: &NetTag) -> Result<ItemStack, CodecError> {
    let Tag::Compound(compound) = net_to_tag(value)? else {
        return Err(CodecError::ExpectedCompound);
    };
    ItemStack::from_nbt(host, &compound).ok_or(CodecError::InvalidValue)
}

// ---------------------------------------------------------------------------
// Packet values.
// ---------------------------------------------------------------------------

/// One slot handler's full state, as sent by `sync_curios`.
#[derive(Clone, Debug, PartialEq, mod_api::PayloadCodec)]
pub struct NetHandler {
    pub curio_id: String,
    pub stacks: Vec<Option<NetTag>>,
    pub cosmetic: Vec<Option<NetTag>>,
    pub render: Vec<bool>,
    pub active: Vec<bool>,
    pub drop_rule: String,
    pub use_native_gui: bool,
}

/// One slot definition, as sent by `sync_data`.
#[derive(Clone, Debug, PartialEq, mod_api::PayloadCodec)]
pub struct NetSlotDeclaration {
    pub id: String,
    pub order: i32,
    pub size: i32,
    pub has_cosmetic: bool,
    pub drop_rule: String,
    pub use_native_gui: bool,
    pub render_toggle: bool,
    pub icon: String,
}

/// One handler's sync tag, as sent by `sync_modifiers`.
#[derive(Clone, Debug, PartialEq, mod_api::PayloadCodec)]
pub struct NetModifier {
    pub curio_id: String,
    pub tag: NetTag,
}

/// Full inventory snapshot (`sync_curios`).
#[derive(Clone, Debug, PartialEq, mod_api::PayloadCodec)]
pub struct SyncCurios {
    pub entity_id: i32,
    pub handlers: Vec<NetHandler>,
}

/// One slot's stack (`sync_stack`). `handler_type` is one of the constants above.
#[derive(Clone, Debug, PartialEq, mod_api::PayloadCodec)]
pub struct SyncStack {
    pub entity_id: i32,
    pub curio_id: String,
    pub slot_id: i32,
    pub stack: Option<NetTag>,
    pub handler_type: u8,
    pub compound_tag: NetTag,
}

/// A stack returned to the client's cursor (`grabbed_item`).
#[derive(Clone, Debug, PartialEq, mod_api::PayloadCodec)]
pub struct GrabbedItem {
    pub stack: Option<NetTag>,
}

/// Loaded slot and entity definitions (`sync_data`).
#[derive(Clone, Debug, PartialEq, mod_api::PayloadCodec)]
pub struct SyncData {
    pub declarations: Vec<NetSlotDeclaration>,
}

/// Handler sync tags (`sync_modifiers`).
#[derive(Clone, Debug, PartialEq, mod_api::PayloadCodec)]
pub struct SyncModifiers {
    pub entity_id: i32,
    pub modifiers: Vec<NetModifier>,
}

/// One render flag (`sync_render`).
#[derive(Clone, Debug, PartialEq, mod_api::PayloadCodec)]
pub struct SyncRender {
    pub entity_id: i32,
    pub curio_id: String,
    pub slot_id: i32,
    pub value: bool,
}

/// One active flag (`sync_active`).
#[derive(Clone, Debug, PartialEq, mod_api::PayloadCodec)]
pub struct SyncActive {
    pub entity_id: i32,
    pub curio_id: String,
    pub slot_id: i32,
    pub value: bool,
}

/// Break animation (`break`).
#[derive(Clone, Debug, PartialEq, mod_api::PayloadCodec)]
pub struct Break {
    pub entity_id: i32,
    pub curio_id: String,
    pub slot_id: i32,
}

/// Menu page acknowledgement (`server_page`).
#[derive(Clone, Debug, PartialEq, mod_api::PayloadCodec)]
pub struct ServerPage {
    pub window_id: i32,
    pub page: i32,
}

/// Deferred quick-move (`quick_move`).
#[derive(Clone, Debug, PartialEq, mod_api::PayloadCodec)]
pub struct QuickMove {
    pub window_id: i32,
    pub move_index: i32,
}

/// The receiving side of a serverbound request, for registration audits.
pub fn client_request_kind(packet: &ClientPacket) -> &'static str {
    match packet {
        ClientPacket::Destroy => channel::DESTROY,
        ClientPacket::OpenCurios { .. } => channel::OPEN_CURIOS,
        ClientPacket::OpenVanilla { .. } => channel::OPEN_VANILLA,
        ClientPacket::ClientPage { .. } => channel::CLIENT_PAGE,
        ClientPacket::ToggleCosmetics { .. } => channel::TOGGLE_COSMETICS,
        ClientPacket::ToggleRender { .. } => channel::TOGGLE_RENDER,
    }
}

/// The receiving side of a clientbound packet, for registration audits.
pub fn server_packet_kind(packet: &ServerPacket) -> &'static str {
    match packet {
        ServerPacket::SyncStack(_) => channel::SYNC_STACK,
        ServerPacket::GrabbedItem(_) => channel::GRABBED_ITEM,
        ServerPacket::SyncCurios(_) => channel::SYNC_CURIOS,
        ServerPacket::SyncData(_) => channel::SYNC_DATA,
        ServerPacket::SyncModifiers(_) => channel::SYNC_MODIFIERS,
        ServerPacket::SyncRender(_) => channel::SYNC_RENDER,
        ServerPacket::SyncActive(_) => channel::SYNC_ACTIVE,
        ServerPacket::Break(_) => channel::BREAK,
        ServerPacket::ServerPage(_) => channel::SERVER_PAGE,
        ServerPacket::QuickMove(_) => channel::QUICK_MOVE,
    }
}

/// Validates a client-supplied slot index against the server's own handler.
/// A client never chooses which slot exists: only a number inside the
/// authoritative inventory is accepted.
fn slot_index(index: i32, len: usize) -> Result<usize, String> {
    if index < 0 {
        return Err(format!("slot index {index} is negative"));
    }
    let index = usize::try_from(index).map_err(|_| format!("slot index {index} is too large"))?;
    if index >= len {
        return Err(format!("slot index {index} is outside the {len} slots"));
    }
    Ok(index)
}

/// All serverbound requests.
#[derive(Clone, Debug, PartialEq, mod_api::PayloadCodec)]
pub enum ClientPacket {
    Destroy,
    OpenCurios { carried: Option<NetTag> },
    OpenVanilla { carried: Option<NetTag> },
    ClientPage { window_id: i32, next: bool },
    ToggleCosmetics { window_id: i32 },
    ToggleRender { identifier: String, index: i32 },
}

/// All clientbound packets.
#[derive(Clone, Debug, PartialEq, mod_api::PayloadCodec)]
pub enum ServerPacket {
    SyncStack(SyncStack),
    GrabbedItem(GrabbedItem),
    SyncCurios(SyncCurios),
    SyncData(SyncData),
    SyncModifiers(SyncModifiers),
    SyncRender(SyncRender),
    SyncActive(SyncActive),
    Break(Break),
    ServerPage(ServerPage),
    QuickMove(QuickMove),
}

// ---------------------------------------------------------------------------
// Channels and tokens.
// ---------------------------------------------------------------------------

/// The sixteen channel names. Typed payloads use one value type per channel,
/// so the token's schema fingerprint is checked on every send.
pub mod channel {
    pub const DESTROY: &str = "curios:destroy";
    pub const OPEN_CURIOS: &str = "curios:open_curios";
    pub const OPEN_VANILLA: &str = "curios:open_vanilla";
    pub const CLIENT_PAGE: &str = "curios:client_page";
    pub const TOGGLE_COSMETICS: &str = "curios:toggle_cosmetics";
    pub const TOGGLE_RENDER: &str = "curios:toggle_render";
    pub const SYNC_STACK: &str = "curios:sync_stack";
    pub const GRABBED_ITEM: &str = "curios:grabbed_item";
    pub const SYNC_CURIOS: &str = "curios:sync_curios";
    pub const SYNC_DATA: &str = "curios:sync_data";
    pub const SYNC_MODIFIERS: &str = "curios:sync_modifiers";
    pub const SYNC_RENDER: &str = "curios:sync_render";
    pub const SYNC_ACTIVE: &str = "curios:sync_active";
    pub const BREAK: &str = "curios:break";
    pub const SERVER_PAGE: &str = "curios:server_page";
    pub const QUICK_MOVE: &str = "curios:quick_move";

    /// All names, for diagnostics and tests.
    pub const ALL: [&str; 16] = [
        DESTROY,
        OPEN_CURIOS,
        OPEN_VANILLA,
        CLIENT_PAGE,
        TOGGLE_COSMETICS,
        TOGGLE_RENDER,
        SYNC_STACK,
        GRABBED_ITEM,
        SYNC_CURIOS,
        SYNC_DATA,
        SYNC_MODIFIERS,
        SYNC_RENDER,
        SYNC_ACTIVE,
        BREAK,
        SERVER_PAGE,
        QUICK_MOVE,
    ];
}

fn id(name: &str) -> PayloadId {
    PayloadId(name.to_owned())
}

macro_rules! token_slot {
    ($name:ident, $type:ty) => {
        static $name: OnceLock<ToClient<$type>> = OnceLock::new();
    };
}

token_slot!(SYNC_STACK_TOKEN, SyncStack);
token_slot!(GRABBED_ITEM_TOKEN, GrabbedItem);
token_slot!(SYNC_CURIOS_TOKEN, SyncCurios);
token_slot!(SYNC_DATA_TOKEN, SyncData);
token_slot!(SYNC_MODIFIERS_TOKEN, SyncModifiers);
token_slot!(SYNC_RENDER_TOKEN, SyncRender);
token_slot!(SYNC_ACTIVE_TOKEN, SyncActive);
token_slot!(BREAK_TOKEN, Break);
token_slot!(SERVER_PAGE_TOKEN, ServerPage);
token_slot!(QUICK_MOVE_TOKEN, QuickMove);

/// A local send or conversion failure. Nothing is silently dropped.
#[derive(Debug)]
pub enum NetError {
    /// A token was not installed because `register` was never called.
    NotRegistered(&'static str),
    /// The player is not online on this server.
    NoPlayer(u128),
    /// The transport refused to queue the payload.
    Send(PayloadSendError),
    /// The attachment store refused a read or write.
    Attachment(AttachmentError),
    /// A value could not be converted to or from NBT.
    Codec(CodecError),
}

impl From<PayloadSendError> for NetError {
    fn from(error: PayloadSendError) -> Self {
        Self::Send(error)
    }
}

impl From<AttachmentError> for NetError {
    fn from(error: AttachmentError) -> Self {
        Self::Attachment(error)
    }
}

impl From<CodecError> for NetError {
    fn from(error: CodecError) -> Self {
        Self::Codec(error)
    }
}

/// The client's last received state. Renderers read this mirror; the authoritative
/// inventory stays on the server.
#[derive(Clone, Debug, Default)]
pub struct ClientState {
    pub handlers: Vec<NetHandler>,
    pub modifiers: Vec<NetModifier>,
    pub declarations: Vec<NetSlotDeclaration>,
    pub page: i32,
    pub last_grabbed: Option<NetTag>,
    pub last_break: Option<Break>,
    pub last_quick_move: Option<QuickMove>,
}

static CLIENT_STATE: Mutex<ClientState> = Mutex::new(ClientState {
    handlers: Vec::new(),
    modifiers: Vec::new(),
    declarations: Vec::new(),
    page: 0,
    last_grabbed: None,
    last_break: None,
    last_quick_move: None,
});

fn client_state() -> std::sync::MutexGuard<'static, ClientState> {
    CLIENT_STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A snapshot of the client's mirror, for rendering and diagnostics.
pub fn client_snapshot() -> ClientState {
    client_state().clone()
}

/// Per-player GUI page state, kept on the server because a client cannot grant it.
static PAGES: Mutex<BTreeMap<u128, i32>> = Mutex::new(BTreeMap::new());

fn pages() -> std::sync::MutexGuard<'static, BTreeMap<u128, i32>> {
    PAGES.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ---------------------------------------------------------------------------
// Registration.
// ---------------------------------------------------------------------------

/// Declares all sixteen payloads. Called from `Hooks::register`.
pub fn register(r: &mut dyn Registrar) {
    let mut view = r.payload_handlers();
    let mut registrar = view.registrar(VERSION).optional();

    registrar.play_to_server_typed::<ClientPacket>(
        id(channel::DESTROY),
        |value, context| handle_client(value, context),
    );
    registrar.play_to_server_typed::<ClientPacket>(
        id(channel::OPEN_CURIOS),
        |value, context| handle_client(value, context),
    );
    registrar.play_to_server_typed::<ClientPacket>(
        id(channel::OPEN_VANILLA),
        |value, context| handle_client(value, context),
    );
    registrar.play_to_server_typed::<ClientPacket>(
        id(channel::CLIENT_PAGE),
        |value, context| handle_client(value, context),
    );
    registrar.play_to_server_typed::<ClientPacket>(
        id(channel::TOGGLE_COSMETICS),
        |value, context| handle_client(value, context),
    );
    registrar.play_to_server_typed::<ClientPacket>(
        id(channel::TOGGLE_RENDER),
        |value, context| handle_client(value, context),
    );

    install(
        &SYNC_STACK_TOKEN,
        registrar.play_to_client_typed::<SyncStack>(id(channel::SYNC_STACK), |value, _| {
            handle_server(ServerPacket::SyncStack(value))
        }),
        channel::SYNC_STACK,
    );
    install(
        &GRABBED_ITEM_TOKEN,
        registrar.play_to_client_typed::<GrabbedItem>(id(channel::GRABBED_ITEM), |value, _| {
            handle_server(ServerPacket::GrabbedItem(value))
        }),
        channel::GRABBED_ITEM,
    );
    install(
        &SYNC_CURIOS_TOKEN,
        registrar.play_to_client_typed::<SyncCurios>(id(channel::SYNC_CURIOS), |value, _| {
            handle_server(ServerPacket::SyncCurios(value))
        }),
        channel::SYNC_CURIOS,
    );
    install(
        &SYNC_DATA_TOKEN,
        registrar.play_to_client_typed::<SyncData>(id(channel::SYNC_DATA), |value, _| {
            handle_server(ServerPacket::SyncData(value))
        }),
        channel::SYNC_DATA,
    );
    install(
        &SYNC_MODIFIERS_TOKEN,
        registrar.play_to_client_typed::<SyncModifiers>(id(channel::SYNC_MODIFIERS), |value, _| {
            handle_server(ServerPacket::SyncModifiers(value))
        }),
        channel::SYNC_MODIFIERS,
    );
    install(
        &SYNC_RENDER_TOKEN,
        registrar.play_to_client_typed::<SyncRender>(id(channel::SYNC_RENDER), |value, _| {
            handle_server(ServerPacket::SyncRender(value))
        }),
        channel::SYNC_RENDER,
    );
    install(
        &SYNC_ACTIVE_TOKEN,
        registrar.play_to_client_typed::<SyncActive>(id(channel::SYNC_ACTIVE), |value, _| {
            handle_server(ServerPacket::SyncActive(value))
        }),
        channel::SYNC_ACTIVE,
    );
    install(
        &BREAK_TOKEN,
        registrar.play_to_client_typed::<Break>(id(channel::BREAK), |value, _| {
            handle_server(ServerPacket::Break(value))
        }),
        channel::BREAK,
    );
    install(
        &SERVER_PAGE_TOKEN,
        registrar.play_to_client_typed::<ServerPage>(id(channel::SERVER_PAGE), |value, _| {
            handle_server(ServerPacket::ServerPage(value))
        }),
        channel::SERVER_PAGE,
    );
    install(
        &QUICK_MOVE_TOKEN,
        registrar.play_to_client_typed::<QuickMove>(id(channel::QUICK_MOVE), |value, _| {
            handle_server(ServerPacket::QuickMove(value))
        }),
        channel::QUICK_MOVE,
    );
}

/// Keeps a token from one registration; a second install is reported, not swallowed.
fn install<T>(slot: &OnceLock<ToClient<T>>, token: ToClient<T>, name: &'static str) {
    if slot.set(token).is_err() {
        eprintln!("[curios] payload token {name} was installed twice");
    }
}

// ---------------------------------------------------------------------------
// Sending (server -> client).
// ---------------------------------------------------------------------------

fn player(host: &dyn ServerHost, uuid: u128) -> Result<mod_api::PlayerRef, NetError> {
    host.player(uuid).ok_or(NetError::NoPlayer(uuid))
}

fn send_to(host: &mut dyn ServerHost, uuid: u128, packet: ServerPacket) -> Result<(), NetError> {
    let target = player(host, uuid)?;
    match packet {
        ServerPacket::SyncStack(value) => {
            SYNC_STACK_TOKEN
                .get()
                .ok_or(NetError::NotRegistered(channel::SYNC_STACK))?
                .send(host, target, &value)?;
        }
        ServerPacket::GrabbedItem(value) => {
            GRABBED_ITEM_TOKEN
                .get()
                .ok_or(NetError::NotRegistered(channel::GRABBED_ITEM))?
                .send(host, target, &value)?;
        }
        ServerPacket::SyncCurios(value) => {
            SYNC_CURIOS_TOKEN
                .get()
                .ok_or(NetError::NotRegistered(channel::SYNC_CURIOS))?
                .send(host, target, &value)?;
        }
        ServerPacket::SyncData(value) => {
            SYNC_DATA_TOKEN
                .get()
                .ok_or(NetError::NotRegistered(channel::SYNC_DATA))?
                .send(host, target, &value)?;
        }
        ServerPacket::SyncModifiers(value) => {
            SYNC_MODIFIERS_TOKEN
                .get()
                .ok_or(NetError::NotRegistered(channel::SYNC_MODIFIERS))?
                .send(host, target, &value)?;
        }
        ServerPacket::SyncRender(value) => {
            SYNC_RENDER_TOKEN
                .get()
                .ok_or(NetError::NotRegistered(channel::SYNC_RENDER))?
                .send(host, target, &value)?;
        }
        ServerPacket::SyncActive(value) => {
            SYNC_ACTIVE_TOKEN
                .get()
                .ok_or(NetError::NotRegistered(channel::SYNC_ACTIVE))?
                .send(host, target, &value)?;
        }
        ServerPacket::Break(value) => {
            BREAK_TOKEN
                .get()
                .ok_or(NetError::NotRegistered(channel::BREAK))?
                .send(host, target, &value)?;
        }
        ServerPacket::ServerPage(value) => {
            SERVER_PAGE_TOKEN
                .get()
                .ok_or(NetError::NotRegistered(channel::SERVER_PAGE))?
                .send(host, target, &value)?;
        }
        ServerPacket::QuickMove(value) => {
            QUICK_MOVE_TOKEN
                .get()
                .ok_or(NetError::NotRegistered(channel::QUICK_MOVE))?
                .send(host, target, &value)?;
        }
    }
    Ok(())
}

/// Number of cosmetic slots, read through the handler's public accessor.
fn cosmetic_len(handler: &SlotStacks) -> usize {
    let mut len = 0;
    while handler.cosmetic(len).is_some() {
        len += 1;
    }
    len
}

/// Converts a slot handler into its wire form.
pub fn net_handler(curio_id: &str, handler: &SlotStacks) -> NetHandler {
    NetHandler {
        curio_id: curio_id.to_owned(),
        stacks: (0..handler.len())
            .map(|index| optional_stack_to_net(handler.stack(index).and_then(Option::as_ref)))
            .collect(),
        cosmetic: (0..cosmetic_len(handler))
            .map(|index| optional_stack_to_net(handler.cosmetic(index).and_then(Option::as_ref)))
            .collect(),
        render: (0..handler.len())
            .filter_map(|index| handler.render(index))
            .collect(),
        active: (0..handler.len())
            .filter_map(|index| handler.active(index))
            .collect(),
        drop_rule: handler.drop_rule().as_str().to_owned(),
        use_native_gui: handler.use_native_gui(),
    }
}

/// Builds a full snapshot without requiring a host.
pub fn snapshot(entity_id: i32, inventory: &CurioInventory) -> SyncCurios {
    SyncCurios {
        entity_id,
        handlers: inventory
            .slot_ids()
            .filter_map(|id| inventory.slot(id).map(|handler| net_handler(id, handler)))
            .collect(),
    }
}

/// Sends the full inventory snapshot (`sync_curios`).
pub fn sync_inventory(
    host: &mut dyn ServerHost,
    uuid: u128,
    inventory: &CurioInventory,
) -> Result<(), NetError> {
    let entity_id = player(host, uuid)?.entity().id() as i32;
    send_to(host, uuid, ServerPacket::SyncCurios(snapshot(entity_id, inventory)))
}

/// Sends one slot's stack (`sync_stack`). `compound` is the item's `canSync` tag.
pub fn sync_stack(
    host: &mut dyn ServerHost,
    uuid: u128,
    curio_id: &str,
    slot_id: i32,
    stack: Option<&ItemSnapshot>,
    handler_type: u8,
    compound: &NetTag,
) -> Result<(), NetError> {
    let entity_id = player(host, uuid)?.entity().id() as i32;
    send_to(
        host,
        uuid,
        ServerPacket::SyncStack(SyncStack {
            entity_id,
            curio_id: curio_id.to_owned(),
            slot_id,
            stack: optional_stack_to_net(stack),
            handler_type,
            compound_tag: compound.clone(),
        }),
    )
}

/// Sends one render flag (`sync_render`).
pub fn sync_render(
    host: &mut dyn ServerHost,
    uuid: u128,
    curio_id: &str,
    slot_id: i32,
    value: bool,
) -> Result<(), NetError> {
    let entity_id = player(host, uuid)?.entity().id() as i32;
    send_to(
        host,
        uuid,
        ServerPacket::SyncRender(SyncRender {
            entity_id,
            curio_id: curio_id.to_owned(),
            slot_id,
            value,
        }),
    )
}

/// Sends one active flag (`sync_active`).
pub fn sync_active(
    host: &mut dyn ServerHost,
    uuid: u128,
    curio_id: &str,
    slot_id: i32,
    value: bool,
) -> Result<(), NetError> {
    let entity_id = player(host, uuid)?.entity().id() as i32;
    send_to(
        host,
        uuid,
        ServerPacket::SyncActive(SyncActive {
            entity_id,
            curio_id: curio_id.to_owned(),
            slot_id,
            value,
        }),
    )
}

/// Sends the loaded slot definitions (`sync_data`).
pub fn sync_data(host: &mut dyn ServerHost, uuid: u128, types: &[SlotType]) -> Result<(), NetError> {
    let declarations = types
        .iter()
        .map(|slot| NetSlotDeclaration {
            id: slot.id().to_owned(),
            order: slot.order(),
            size: slot.size(),
            has_cosmetic: slot.has_cosmetic(),
            drop_rule: slot.drop_rule().as_str().to_owned(),
            use_native_gui: slot.use_native_gui(),
            render_toggle: slot.render_toggle(),
            icon: slot.icon().as_str().to_string(),
        })
        .collect();
    send_to(host, uuid, ServerPacket::SyncData(SyncData { declarations }))
}

/// Sends handler sync tags (`sync_modifiers`).
pub fn sync_modifiers(
    host: &mut dyn ServerHost,
    uuid: u128,
    modifiers: Vec<NetModifier>,
) -> Result<(), NetError> {
    let entity_id = player(host, uuid)?.entity().id() as i32;
    send_to(
        host,
        uuid,
        ServerPacket::SyncModifiers(SyncModifiers {
            entity_id,
            modifiers,
        }),
    )
}

/// Requests a break animation on the client (`break`).
pub fn break_effect(
    host: &mut dyn ServerHost,
    uuid: u128,
    curio_id: &str,
    slot_id: i32,
) -> Result<(), NetError> {
    let entity_id = player(host, uuid)?.entity().id() as i32;
    send_to(
        host,
        uuid,
        ServerPacket::Break(Break {
            entity_id,
            curio_id: curio_id.to_owned(),
            slot_id,
        }),
    )
}

/// Sets the acknowledged menu page (`server_page`).
pub fn server_page(host: &mut dyn ServerHost, uuid: u128, window_id: i32, page: i32) -> Result<(), NetError> {
    send_to(host, uuid, ServerPacket::ServerPage(ServerPage { window_id, page }))
}

/// Defers a quick-move after a slot reset (`quick_move`).
pub fn quick_move(
    host: &mut dyn ServerHost,
    uuid: u128,
    window_id: i32,
    move_index: i32,
) -> Result<(), NetError> {
    send_to(
        host,
        uuid,
        ServerPacket::QuickMove(QuickMove {
            window_id,
            move_index,
        }),
    )
}

/// Returns a stack to the client's cursor (`grabbed_item`).
pub fn grabbed_item(
    host: &mut dyn ServerHost,
    uuid: u128,
    stack: Option<&ItemSnapshot>,
) -> Result<(), NetError> {
    send_to(
        host,
        uuid,
        ServerPacket::GrabbedItem(GrabbedItem {
            stack: optional_stack_to_net(stack),
        }),
    )
}

// ---------------------------------------------------------------------------
// Receiving (client -> server, and client-side mirror updates).
// ---------------------------------------------------------------------------

/// Reads the acting player's inventory, reporting every failure.
fn read_inventory(
    host: &mut dyn ServerHost,
    uuid: u128,
) -> Result<crate::inventory::CurioInventory, NetError> {
    let entity_id = player(host, uuid)?.entity().id();
    crate::inventory::INVENTORY
        .get(host, mod_api::Holder::Entity(entity_id))
        .map_err(NetError::from)
}

/// Writes a player's inventory back, reporting every failure.
fn write_inventory(
    host: &mut dyn ServerHost,
    uuid: u128,
    inventory: crate::inventory::CurioInventory,
) -> Result<(), NetError> {
    let entity_id = player(host, uuid)?.entity().id();
    crate::inventory::INVENTORY
        .set(host, mod_api::Holder::Entity(entity_id), inventory)
        .map_err(NetError::from)
}

/// Applies every client request. All mutations happen here, on the server.
pub fn handle_client(
    request: ClientPacket,
    context: &mut dyn IPayloadContext,
) -> Result<(), String> {
    let actor = context
        .player()
        .map_err(|error| format!("curios payload has no player: {error:?}"))?;
    let host = context
        .server_host()
        .map_err(|error| format!("curios payload needs the server host: {error:?}"))?;
    let uuid = actor.uuid();

    match request {
        ClientPacket::Destroy => {
            let mut inventory = read_inventory(host, uuid).map_err(|error| format!("{error:?}"))?;
            let ids: Vec<String> = inventory.slot_ids().map(str::to_owned).collect();
            for id in ids {
                let handler = inventory
                    .slot_mut(&id)
                    .ok_or_else(|| format!("slot type {id} disappeared during destroy"))?;
                for index in 0..handler.len() {
                    handler
                        .set_stack(index, None)
                        .map_err(|error| format!("{error:?}"))?;
                }
                for index in 0..cosmetic_len(handler) {
                    handler
                        .set_cosmetic(index, None)
                        .map_err(|error| format!("{error:?}"))?;
                }
            }
            write_inventory(host, uuid, inventory.clone()).map_err(|error| format!("{error:?}"))?;
            sync_inventory(host, uuid, &inventory).map_err(|error| format!("{error:?}"))?;
        }
        ClientPacket::OpenCurios { carried } | ClientPacket::OpenVanilla { carried } => {
            if let Some(tag) = carried {
                let stack = net_to_stack(host, &tag)
                    .map_err(|error| format!("curios carried stack was refused: {error:?}"))?;
                // Validation is authoritative; the client keeps its cursor and the
                // menu opens client-side.
                drop(stack);
                grabbed_item(host, uuid, None).map_err(|error| format!("{error:?}"))?;
            }
            pages().insert(uuid, 0);
            server_page(host, uuid, 0, 0).map_err(|error| format!("{error:?}"))?;
        }
        ClientPacket::ClientPage { window_id, next } => {
            let current = *pages().get(&uuid).unwrap_or(&0);
            let page = if next { current + 1 } else { current.saturating_sub(1) };
            pages().insert(uuid, page);
            server_page(host, uuid, window_id, page).map_err(|error| format!("{error:?}"))?;
        }
        ClientPacket::ToggleCosmetics { window_id } => {
            let inventory = read_inventory(host, uuid).map_err(|error| format!("{error:?}"))?;
            sync_inventory(host, uuid, &inventory).map_err(|error| format!("{error:?}"))?;
            let page = *pages().get(&uuid).unwrap_or(&0);
            server_page(host, uuid, window_id, page).map_err(|error| format!("{error:?}"))?;
        }
        ClientPacket::ToggleRender { identifier, index } => {
            let mut inventory = read_inventory(host, uuid).map_err(|error| format!("{error:?}"))?;
            let handler = inventory
                .slot_mut(&identifier)
                .ok_or_else(|| format!("curios slot {identifier} is not equipped by {uuid}"))?;
            let slot = slot_index(index, handler.len())?;
            let old = handler
                .render(slot)
                .ok_or_else(|| format!("curios slot {identifier} has no index {index}"))?;
            let value = !old;
            handler
                .set_render(slot, value)
                .map_err(|error| format!("{error:?}"))?;
            write_inventory(host, uuid, inventory).map_err(|error| format!("{error:?}"))?;
            // Collect the targets first: `server_players()` borrows the host for
            // the iterator's lifetime, while a send needs `&mut`.
            let targets: Vec<u128> = host
                .players()
                .server_players()
                .into_iter()
                .map(|other| other.uuid())
                .collect();
            for other_uuid in targets {
                if let Err(error) = sync_render(host, other_uuid, &identifier, index, value) {
                    eprintln!("[curios] sync_render to {other_uuid} failed: {error:?}");
                }
            }
        }
    }
    Ok(())
}

/// Applies one server packet to the client's render mirror.
pub fn handle_server(packet: ServerPacket) -> Result<(), String> {
    match packet {
        ServerPacket::SyncCurios(value) => {
            client_state().handlers = value.handlers;
            Ok(())
        }
        ServerPacket::SyncStack(value) => {
            let mut state = client_state();
            let Some(handler) = state
                .handlers
                .iter_mut()
                .find(|handler| handler.curio_id == value.curio_id)
            else {
                return Err(format!("unknown curio id {}", value.curio_id));
            };
            let count = if value.handler_type == HANDLER_COSMETIC {
                handler.cosmetic.len()
            } else {
                handler.stacks.len()
            };
            let index = slot_index(value.slot_id, count)?;
            let list = if value.handler_type == HANDLER_COSMETIC {
                &mut handler.cosmetic
            } else {
                &mut handler.stacks
            };
            let slot = list
                .get_mut(index)
                .ok_or_else(|| format!("slot {index} of {} is not synced", value.curio_id))?;
            *slot = value.stack;
            Ok(())
        }
        ServerPacket::SyncData(value) => {
            client_state().declarations = value.declarations;
            Ok(())
        }
        ServerPacket::SyncModifiers(value) => {
            client_state().modifiers = value.modifiers;
            Ok(())
        }
        ServerPacket::SyncRender(value) => {
            let mut state = client_state();
            let Some(handler) = state
                .handlers
                .iter_mut()
                .find(|handler| handler.curio_id == value.curio_id)
            else {
                return Err(format!("unknown curio id {}", value.curio_id));
            };
            let index = slot_index(value.slot_id, handler.render.len())?;
            let slot = handler
                .render
                .get_mut(index)
                .ok_or_else(|| format!("render flag {index} of {} is not synced", value.curio_id))?;
            *slot = value.value;
            Ok(())
        }
        ServerPacket::SyncActive(value) => {
            let mut state = client_state();
            let Some(handler) = state
                .handlers
                .iter_mut()
                .find(|handler| handler.curio_id == value.curio_id)
            else {
                return Err(format!("unknown curio id {}", value.curio_id));
            };
            let index = slot_index(value.slot_id, handler.active.len())?;
            let slot = handler
                .active
                .get_mut(index)
                .ok_or_else(|| format!("active flag {index} of {} is not synced", value.curio_id))?;
            *slot = value.value;
            Ok(())
        }
        ServerPacket::GrabbedItem(value) => {
            client_state().last_grabbed = value.stack;
            Ok(())
        }
        ServerPacket::Break(value) => {
            client_state().last_break = Some(value);
            Ok(())
        }
        ServerPacket::ServerPage(value) => {
            client_state().page = value.page;
            Ok(())
        }
        ServerPacket::QuickMove(value) => {
            client_state().last_quick_move = Some(value);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mod_api::{decode_payload, encode_payload};

    #[test]
    fn every_tag_variant_survives_the_wire() {
        let values = vec![
            NetTag::End,
            NetTag::Byte(-1),
            NetTag::Short(-2),
            NetTag::Int(-3),
            NetTag::Long(-4),
            NetTag::Float(5),
            NetTag::Double(6),
            NetTag::ByteArray(vec![1, 2, 3]),
            NetTag::String("curios:ring".to_owned()),
            NetTag::List(vec![NetTag::Int(1), NetTag::End]),
            NetTag::Compound(vec![NetEntry {
                key: "id".to_owned(),
                value: NetTag::String("minecraft:stone".to_owned()),
            }]),
            NetTag::IntArray(vec![1, 2]),
            NetTag::LongArray(vec![3, 4]),
        ];
        for value in values {
            let body = encode_payload(&value, 4096).unwrap();
            assert_eq!(decode_payload::<NetTag>(&body), Ok(value));
        }
    }

    #[test]
    fn nbt_and_wire_forms_round_trip() {
        let mut nbt = Compound::new();
        nbt.insert("id".into(), Tag::String("curios:ring".into()));
        nbt.insert("count".into(), Tag::Byte(2));
        nbt.insert("tag".into(), Tag::Compound(Compound::new()));
        let wire = tag_to_net(&Tag::Compound(nbt.clone()));
        assert_eq!(net_to_tag(&wire), Ok(Tag::Compound(nbt)));
    }

    #[test]
    fn a_full_snapshot_round_trips_through_the_payload_codec() {
        let types = crate::registry::slot_types();
        let mut inventory = CurioInventory::from_slot_types(&types);
        let handler = inventory.slot_mut("ring").unwrap();
        handler.set_render(0, false).unwrap();
        handler.set_active(0, false).unwrap();
        let packet = ServerPacket::SyncCurios(snapshot(7, &inventory));
        let body = encode_payload(&packet, 1 << 20).unwrap();
        let decoded = decode_payload::<ServerPacket>(&body).unwrap();
        assert_eq!(decoded, packet);
    }

    #[test]
    fn a_stack_update_keeps_its_handler_type() {
        let packet = ClientPacket::ToggleRender {
            identifier: "ring".to_owned(),
            index: 3,
        };
        let body = encode_payload(&packet, 1024).unwrap();
        assert_eq!(
            decode_payload::<ClientPacket>(&body),
            Ok(ClientPacket::ToggleRender {
                identifier: "ring".to_owned(),
                index: 3,
            })
        );
        let empty = ServerPacket::SyncStack(SyncStack {
            entity_id: 1,
            curio_id: "ring".to_owned(),
            slot_id: 0,
            stack: None,
            handler_type: HANDLER_EQUIPMENT,
            compound_tag: NetTag::Compound(Vec::new()),
        });
        let body = encode_payload(&empty, 1024).unwrap();
        assert_eq!(decode_payload::<ServerPacket>(&body), Ok(empty));
    }

    #[test]
    fn all_sixteen_channels_are_distinct_and_namespaced() {
        let mut names = std::collections::BTreeSet::new();
        for name in channel::ALL {
            assert!(name.starts_with("curios:"), "{name}");
            assert!(names.insert(name), "duplicate channel {name}");
        }
        assert_eq!(names.len(), 16);
    }

    #[test]
    fn every_channel_has_a_receiving_handler() {
        let requests = [
            ClientPacket::Destroy,
            ClientPacket::OpenCurios { carried: None },
            ClientPacket::OpenVanilla { carried: None },
            ClientPacket::ClientPage {
                window_id: 0,
                next: true,
            },
            ClientPacket::ToggleCosmetics { window_id: 0 },
            ClientPacket::ToggleRender {
                identifier: "ring".to_owned(),
                index: 0,
            },
        ];
        let updates = [
            ServerPacket::SyncStack(SyncStack {
                entity_id: 1,
                curio_id: "ring".to_owned(),
                slot_id: 0,
                stack: None,
                handler_type: HANDLER_EQUIPMENT,
                compound_tag: NetTag::Compound(Vec::new()),
            }),
            ServerPacket::GrabbedItem(GrabbedItem { stack: None }),
            ServerPacket::SyncCurios(SyncCurios {
                entity_id: 1,
                handlers: Vec::new(),
            }),
            ServerPacket::SyncData(SyncData {
                declarations: Vec::new(),
            }),
            ServerPacket::SyncModifiers(SyncModifiers {
                entity_id: 1,
                modifiers: Vec::new(),
            }),
            ServerPacket::SyncRender(SyncRender {
                entity_id: 1,
                curio_id: "ring".to_owned(),
                slot_id: 0,
                value: true,
            }),
            ServerPacket::SyncActive(SyncActive {
                entity_id: 1,
                curio_id: "ring".to_owned(),
                slot_id: 0,
                value: true,
            }),
            ServerPacket::Break(Break {
                entity_id: 1,
                curio_id: "ring".to_owned(),
                slot_id: 0,
            }),
            ServerPacket::ServerPage(ServerPage {
                window_id: 0,
                page: 0,
            }),
            ServerPacket::QuickMove(QuickMove {
                window_id: 0,
                move_index: 1,
            }),
        ];
        let mut handled = std::collections::BTreeSet::new();
        for request in &requests {
            handled.insert(client_request_kind(request));
        }
        for update in &updates {
            handled.insert(server_packet_kind(update));
        }
        assert_eq!(handled.len(), 16);
        for name in channel::ALL {
            assert!(handled.contains(name), "channel {name} has no handler");
        }
    }

    #[test]
    fn client_supplied_slot_indices_are_validated() {
        assert_eq!(slot_index(0, 3), Ok(0));
        assert_eq!(slot_index(2, 3), Ok(2));
        assert!(slot_index(-1, 3).is_err());
        assert!(slot_index(3, 3).is_err());
        assert!(slot_index(9, 3).is_err());
    }

    #[test]
    fn truncated_and_random_payload_bodies_are_refused() {
        let value = NetTag::Compound(vec![
            NetEntry {
                key: "id".to_owned(),
                value: NetTag::String("minecraft:stone".to_owned()),
            },
            NetEntry {
                key: "counts".to_owned(),
                value: NetTag::List(vec![NetTag::Int(1), NetTag::Int(2)]),
            },
        ]);
        let body = encode_payload(&value, 4096).unwrap();
        assert!(body.len() > 4);
        for cut in 0..body.len() {
            assert!(
                decode_payload::<NetTag>(&body[..cut]).is_err(),
                "a body truncated to {cut} bytes was accepted"
            );
        }
        for seed in 0..64u8 {
            let bytes: Vec<u8> = (0..16).map(|i| seed.wrapping_add(i)).collect();
            assert!(decode_payload::<NetTag>(&bytes).is_err());
        }
        let mut trailing = body.clone();
        trailing.push(0);
        assert!(decode_payload::<NetTag>(&trailing).is_err());
    }

    #[test]
    fn a_truncated_client_request_is_never_dispatched() {
        let request = ClientPacket::ToggleRender {
            identifier: "ring".to_owned(),
            index: 2,
        };
        let body = encode_payload(&request, 1024).unwrap();
        for cut in 0..body.len() {
            assert!(
                decode_payload::<ClientPacket>(&body[..cut]).is_err(),
                "truncated client request at {cut} decoded"
            );
        }
        let mut corrupt = body;
        corrupt[0] = 0xff; // no such variant tag
        assert!(decode_payload::<ClientPacket>(&corrupt).is_err());
    }

    #[test]
    fn the_client_mirror_applies_single_slot_updates() {
        let handler = NetHandler {
            curio_id: "ring".to_owned(),
            stacks: vec![None, None],
            cosmetic: Vec::new(),
            render: vec![true, true],
            active: vec![true, true],
            drop_rule: "default".to_owned(),
            use_native_gui: false,
        };
        handle_server(ServerPacket::SyncCurios(SyncCurios {
            entity_id: 1,
            handlers: vec![handler],
        }))
        .unwrap();
        handle_server(ServerPacket::SyncRender(SyncRender {
            entity_id: 1,
            curio_id: "ring".to_owned(),
            slot_id: 1,
            value: false,
        }))
        .unwrap();
        let state = client_snapshot();
        assert_eq!(state.handlers[0].render, vec![true, false]);
        assert!(handle_server(ServerPacket::SyncRender(SyncRender {
            entity_id: 1,
            curio_id: "missing".to_owned(),
            slot_id: 0,
            value: false,
        }))
        .is_err());
    }
}
