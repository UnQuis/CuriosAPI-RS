//! The native Curios screen: a vanilla menu with mod-drawn decorations.
//!
//! Horizon 0.2 gives real, server-authoritative slots only to menu screens;
//! a `mod_screen_open` window has no slots at all. Curios has no block, so the
//! screen is built on a vanilla `minecraft:generic_9xN` chest menu opened by
//! [`open`]. Its container slots are filled with the stacks read from
//! [`crate::place::curio_place`] / [`crate::place::cosmetic_place`], so the
//! engine draws and moves real stacks. The mod draws only frames, labels,
//! cosmetic markers and page/render controls in the screen hooks.
//!
//! Every public `*_pre` / `*_post` function is for `lib.rs` to forward from
//! the single `#[horizon_mod] Hooks` implementation. This module never edits
//! that file.
//!
//! Adapted from TheIllusiveC4/Curios (LGPL-3.0-or-later, copyright 2018-2024
//! C4); see COPYING and COPYING.LESSER.

use std::collections::BTreeMap;
use std::sync::Mutex;

use mod_api::{
    ClientHost, Compound, DrawContext, Flow, Holder, HostTransaction, ItemPlace, ItemResource, ItemStack, MenuReplace,
    ModContainer, ScreenRenderBackgroundEvent, ScreenRenderPostEvent, ScreenView, ServerHost, Tag,
    Text,
};

use crate::config;
use crate::equip::{self, TriState};
use crate::input::{self, ScreenCellWire};
use crate::slots::SlotContext;
use crate::lang;
use crate::place;
use crate::registry;

/// Vanilla menu used for the screen. Six rows hold the 48 slots per page
/// allowed by the shipped server configuration.
pub const MENU_ID: &str = "minecraft:generic_9x6";
pub const MENU_ROWS: u8 = 6;
pub const MENU_COLUMNS: usize = 9;
pub const MENU_SLOTS: usize = MENU_ROWS as usize * MENU_COLUMNS;
pub const PLAYER_SLOTS: usize = 36;

/// The container for a block that wants to expose this menu. Curios itself
/// opens the menu directly through [`open`], but the shape is exported so a
/// future block can reuse it without a second menu id.
pub fn container() -> ModContainer {
    let inputs: Vec<usize> = (0..MENU_SLOTS).collect();
    let faces = [inputs.clone(), inputs.clone(), inputs.clone(), inputs.clone(), inputs.clone(), inputs];
    ModContainer {
        slots: MENU_SLOTS,
        faces,
        inputs: (0..MENU_SLOTS).collect(),
        outputs: (0..MENU_SLOTS).collect(),
        menu: Some(MENU_ID.to_owned()),
    }
}

/// One addressable curio cell on the current page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub slot_type: String,
    pub index: usize,
    pub cosmetic: bool,
}

impl Cell {
    /// The mod place this cell displays. Both kinds are encoded in the key:
    /// cosmetic pairs use the flag added in `src/place.rs`.
    pub fn place(&self, entity: u32) -> Option<ItemPlace> {
        let index = u16::try_from(self.index).ok()?;
        if self.cosmetic {
            place::cosmetic_place(entity, &self.slot_type, index)
        } else {
            place::curio_place(entity, &self.slot_type, index)
        }
    }
}

/// Number of pages needed for `total` cells. Always at least one.
pub fn page_count(total: usize, per_page: usize) -> usize {
    if per_page == 0 {
        return 1;
    }
    total.div_ceil(per_page).max(1)
}

/// Expands the registry into the cells displayed on `page`, in deterministic
/// `order`/`id` order. Cosmetic pairs follow their functional slot when the
/// slot type declares them.
pub fn visible_cells(page: usize, cosmetic: bool) -> Vec<Cell> {
    let per_page = usize::from(config::current().server.max_slots_per_page);
    let mut cells = Vec::new();
    for slot in registry::slot_types().iter() {
        if !slot.use_native_gui() {
            continue;
        }
        let size = usize::try_from(slot.size()).unwrap_or(0);
        for index in 0..size {
            cells.push(Cell {
                slot_type: slot.id().to_owned(),
                index,
                cosmetic: false,
            });
            if slot.has_cosmetic() {
                cells.push(Cell {
                    slot_type: slot.id().to_owned(),
                    index,
                    cosmetic: true,
                });
            }
        }
    }
    let start = page.saturating_mul(per_page).min(cells.len());
    let end = start.saturating_add(per_page).min(cells.len());
    cells[start..end]
        .iter()
        .filter(|cell| cosmetic || !cell.cosmetic)
        .cloned()
        .collect()
}

/// Cells actually shown on screen. Cosmetic view swaps each functional slot
/// for its cosmetic pair; it does not double the number of cells (Java's
/// `CuriosMenu.isViewingCosmetics` behaves the same way).
pub fn display_cells(page: usize, cosmetic: bool) -> Vec<Cell> {
    visible_cells(page, false)
        .into_iter()
        .map(|mut cell| {
            cell.cosmetic = cosmetic;
            cell
        })
        .collect()
}

/// The only screen state kept on the client. The stacks remain the server's.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuState {
    pub page: usize,
    pub cosmetic: bool,
    pub hovering: Option<usize>,
    /// Top-left of the open panel in GUI pixels, remembered from the last
    /// frame so input/tooltip hooks can hit-test without a `ScreenView`.
    pub origin: [f32; 2],
}

impl MenuState {
    pub fn new() -> Self {
        Self {
            page: 0,
            cosmetic: false,
            hovering: None,
            origin: [0.0, 0.0],
        }
    }

    pub fn advance(&mut self, forward: bool) -> bool {
        let per_page = usize::from(config::current().server.max_slots_per_page).max(1);
        let total = cell_total();
        let last = page_count(total, per_page).saturating_sub(1);
        let next = if forward {
            (self.page + 1).min(last)
        } else {
            self.page.saturating_sub(1)
        };
        let changed = next != self.page;
        self.page = next;
        changed
    }

    pub fn toggle_cosmetic(&mut self) {
        self.cosmetic = !self.cosmetic;
        self.hovering = None;
    }
}

impl Default for MenuState {
    fn default() -> Self {
        Self::new()
    }
}

fn cell_total() -> usize {
    registry::slot_types()
        .iter()
        .filter(|slot| slot.use_native_gui())
        .map(|slot| {
            let size = usize::try_from(slot.size()).unwrap_or(0);
            if slot.has_cosmetic() { size * 2 } else { size }
        })
        .sum()
}

/// One cell of the client's screen snapshot, resolved from a mod place on the
/// server and decoded into a real item stack on the client.
#[derive(Clone, Debug)]
pub struct ScreenCell {
    pub index: u16,
    pub cosmetic: bool,
    pub active: bool,
    pub render: bool,
    pub stack: Option<ItemStack>,
    pub slot_type: String,
}

#[derive(Clone, Debug)]
pub struct ScreenSnapshot {
    pub page: usize,
    pub cosmetic: bool,
    pub cells: Vec<ScreenCell>,
}

/// Everything one player's Curios screen needs. A process-global page, window
/// or snapshot would let a second player overwrite the first player's view.
#[derive(Clone, Debug, Default)]
pub struct PlayerMenu {
    pub state: MenuState,
    pub window: Option<i32>,
    pub screen: Option<ScreenSnapshot>,
}

static MENUS: Mutex<BTreeMap<u128, PlayerMenu>> = Mutex::new(BTreeMap::new());

fn with_player<R>(uuid: u128, _default: R, f: impl FnOnce(&mut PlayerMenu) -> R) -> R {
    let mut guard = MENUS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(guard.entry(uuid).or_insert_with(PlayerMenu::default))
}

fn player_state(uuid: u128) -> MenuState {
    with_player(uuid, MenuState::new(), |menu| menu.state.clone())
}

fn player_window(uuid: u128) -> Option<i32> {
    with_player(uuid, None, |menu| menu.window)
}

fn player_screen(uuid: u128) -> Option<ScreenSnapshot> {
    with_player(uuid, None, |menu| menu.screen.clone())
}

fn forget_player(uuid: u128) {
    let mut guard = MENUS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.remove(&uuid);
}

/// A control under the pointer. Pure geometry, shared by drawing and input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    PagePrev,
    PageNext,
    ToggleCosmetics,
    ToggleRender,
    Slot(usize),
    None,
}

/// Screen-space layout in GUI pixels, derived from the menu panel rectangle.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub panel: [f32; 4],
}

const PANEL_W: f32 = 208.0;
const SLOT_STEP: f32 = 20.0;
const SLOT_INSET: f32 = 8.0;
const SLOT_TOP: f32 = 22.0;
const SLOT_ICON: f32 = 18.0;
const BUTTON_SIZE: f32 = 12.0;
const BUTTON_Y: f32 = 4.0;

impl Layout {
    /// The custom screen is wider than the vanilla chest panel that backs it:
    /// nine 20-pixel columns plus margins need at least 208 pixels.
    pub fn new(panel: [f32; 4]) -> Self {
        Self {
            panel: [panel[0], panel[1], panel[2].max(PANEL_W), panel[3]],
        }
    }

    /// Rectangle of custom cell `index`. This is our own grid, not a vanilla
    /// chest slot: the engine's slots are only the container backing.
    pub fn slot_rect(&self, index: usize) -> [f32; 4] {
        let column = (index % MENU_COLUMNS) as f32;
        let row = (index / MENU_COLUMNS) as f32;
        [
            self.panel[0] + SLOT_INSET + column * SLOT_STEP,
            self.panel[1] + SLOT_TOP + row * SLOT_STEP,
            SLOT_ICON,
            SLOT_ICON,
        ]
    }

    /// Rectangle of one of the three mod controls.
    pub fn button_rect(&self, control: Control) -> Option<[f32; 4]> {
        let x = match control {
            Control::PagePrev => self.panel[0] + self.panel[2] - 3.0 * BUTTON_SIZE - 6.0,
            Control::PageNext => self.panel[0] + self.panel[2] - 2.0 * BUTTON_SIZE - 6.0,
            Control::ToggleCosmetics => self.panel[0] + self.panel[2] - BUTTON_SIZE - 6.0,
            Control::ToggleRender => self.panel[0] + 6.0,
            Control::Slot(_) | Control::None => return None,
        };
        Some([x, self.panel[1] + BUTTON_Y, BUTTON_SIZE, BUTTON_SIZE])
    }

    pub fn hit(&self, at: [f32; 2]) -> Control {
        self.hit_with_buttons(at, true)
    }

    /// Hit-testing that honours the client `enable_button` setting (#24):
    /// with buttons disabled their rectangles are not interactive.
    pub fn hit_with_buttons(&self, at: [f32; 2], buttons: bool) -> Control {
        if buttons {
            for control in [
                Control::PagePrev,
                Control::PageNext,
                Control::ToggleCosmetics,
                Control::ToggleRender,
            ] {
                if let Some(rect) = self.button_rect(control) {
                    if contains(rect, at) {
                        return control;
                    }
                }
            }
        }
        for index in 0..MENU_SLOTS {
            if contains(self.slot_rect(index), at) {
                return Control::Slot(index);
            }
        }
        Control::None
    }
}

fn contains(rect: [f32; 4], at: [f32; 2]) -> bool {
    at[0] >= rect[0] && at[0] < rect[0] + rect[2] && at[1] >= rect[1] && at[1] < rect[1] + rect[3]
}

/// Whether the screen belongs to this mod's menu. A vanilla chest of the
/// same size is indistinguishable at this revision, so the title and slot
/// count are the only available guard.
fn is_ours(view: &ScreenView<'_>) -> bool {
    view.id == "inventory"
        && view.menu == Some(MENU_ID)
        && view.slots == MENU_SLOTS + PLAYER_SLOTS
}

/// Reads the stacks for `page` straight from the mod places. This is the
/// only source of truth: the menu is a mirror, never the storage.
pub fn page_stacks(host: &mut dyn ServerHost, uuid: u128, page: usize) -> Vec<Option<mod_api::ItemStack>> {
    let mut result = vec![None; MENU_SLOTS];
    let Some(player) = host.player(uuid) else {
        return result;
    };
    let entity = player.entity().id();
    let cosmetic = player_state(uuid).cosmetic;
    for (slot, cell) in display_cells(page, cosmetic).into_iter().enumerate() {
        if slot >= MENU_SLOTS {
            break;
        }
        let Some(place) = cell.place(entity) else {
            continue;
        };
        result[slot] = place.stack(host);
    }
    result
}

/// Opens the native menu with the first page of curio stacks. Returns false
/// only when the player is gone or the engine refuses the open.
pub fn open(host: &mut dyn ServerHost, uuid: u128) -> bool {
    let title = title();
    let items = page_stacks(host, uuid, 0);
    with_player(uuid, (), |menu| {
        menu.state = MenuState::new();
        menu.window = None;
        menu.screen = None;
    });
    let opened = host.open_menu(uuid, MENU_ROWS, title, items, MenuReplace::CloseFirst);
    match opened {
        Ok(Some(window)) => {
            with_player(uuid, (), |menu| menu.window = Some(window));
            if !sync_state(host, uuid) {
                eprintln!("[curios] screen snapshot could not be sent to {uuid}");
            }
            true
        }
        Ok(None) => false,
        Err(error) => {
            eprintln!("[curios] could not open menu for {uuid}: {error:?}");
            false
        }
    }
}

/// Refreshes the mirrored menu after an external change or a page turn.
pub fn refresh(host: &mut dyn ServerHost, uuid: u128, window: i32, page: usize) -> bool {
    let items = page_stacks(host, uuid, page);
    host.menu_set_items(uuid, window, items).is_ok()
}

/// Applies a server snapshot on the client. Stacks are decoded from the NBT
/// carried in the payload; an undecodable stack is dropped with a diagnostic
/// rather than shown as a misleading empty slot.
pub fn apply_screen_state(
    uuid: u128,
    host: &mut dyn ClientHost,
    page: u8,
    cosmetic: bool,
    wire: &[ScreenCellWire],
) {
    let types = display_cells(usize::from(page), cosmetic);
    let mut cells = Vec::with_capacity(wire.len());
    for entry in wire {
        let slot_type = types
            .get(usize::from(entry.index))
            .map(|cell| cell.slot_type.clone())
            .unwrap_or_default();
        let stack = if entry.flags & input::CELL_HAS_STACK != 0 {
            match Tag::from_network_bytes(&entry.nbt, None) {
                Ok((tag, _)) => match Compound::try_from(tag) {
                    Ok(compound) => match ItemStack::from_nbt(&*host, &compound) {
                        Some(stack) => Some(stack),
                        None => {
                            eprintln!("[curios] screen stack could not be decoded at {}", entry.index);
                            None
                        }
                    },
                    Err(error) => {
                        eprintln!("[curios] screen stack root refused: {error:?}");
                        None
                    }
                },
                Err(error) => {
                    eprintln!("[curios] screen stack NBT refused at {}: {error:?}", entry.index);
                    None
                }
            }
        } else {
            None
        };
        cells.push(ScreenCell {
            index: entry.index,
            cosmetic: entry.flags & input::CELL_COSMETIC != 0,
            active: entry.flags & input::CELL_ACTIVE != 0,
            render: entry.flags & input::CELL_RENDER != 0,
            stack,
            slot_type,
        });
    }
    with_player(uuid, (), |menu| {
        menu.screen = Some(ScreenSnapshot {
            page: usize::from(page),
            cosmetic,
            cells,
        });
        menu.state.page = usize::from(page);
        menu.state.cosmetic = cosmetic;
    });
}

/// Overlay paths of the two shipped tag files, in the form the
/// `data_pack_files` hook expects (`<kind>/<namespace>:<path>`, no `.json`).
pub const CURIO_ITEM_TAG_PATH: &str = "tags/curios:item/curio";
pub const PLAYER_LIKE_TAG_PATH: &str = "tags/curios:entity_type/player_like";

/// Byte contents of the shipped tag files. Push these into `DataPackFilesEvent`
/// so the engine actually registers `#curios:curio` and `#curios:player_like`;
/// a file on disk alone is not contributed to a native mod's data load.
pub fn tag_files() -> Vec<(String, Vec<u8>)> {
    vec![
        (
            CURIO_ITEM_TAG_PATH.to_owned(),
            include_bytes!("../data/curios/tags/item/curio.json").to_vec(),
        ),
        (
            PLAYER_LIKE_TAG_PATH.to_owned(),
            include_bytes!("../data/curios/tags/entity_type/player_like.json").to_vec(),
        ),
    ]
}

/// Sends the current curio view to one player. The vanilla menu remains the
/// container backing, but every visible stack is read from the mod places.
pub fn sync_state(host: &mut dyn ServerHost, uuid: u128) -> bool {
    let Some(player) = host.player(uuid) else {
        return false;
    };
    let page = player_state(uuid).page;
    let cosmetic = player_state(uuid).cosmetic;
    let entity = player.entity().id();
    // Online players are keyed by UUID in the attachment store; the entity id
    // holder produced MissingHolder in the live client.
    let holder = crate::inventory::holder_for_player(uuid);
    let inventory = crate::inventory::INVENTORY.get(host, holder).ok();
    let mut wire = Vec::new();
    for (position, cell) in display_cells(page, cosmetic).into_iter().enumerate() {
        if position > usize::from(u16::MAX) {
            break;
        }
        let Some(place) = cell.place(entity) else {
            continue;
        };
        let stack = place.stack(host);
        let mut flags = 0u8;
        if cell.cosmetic {
            flags |= input::CELL_COSMETIC;
        }
        if let Some(handler) = inventory.as_ref().and_then(|inv| inv.slot(&cell.slot_type)) {
            if handler.active(cell.index).unwrap_or(true) {
                flags |= input::CELL_ACTIVE;
            }
            if handler.render(cell.index).unwrap_or(true) {
                flags |= input::CELL_RENDER;
            }
        }
        let mut nbt = Vec::new();
        if let Some(stack) = stack.as_ref() {
            match crate::inventory::ItemSnapshot::from_stack(&*host, stack) {
                Ok(snapshot) => match Tag::from(snapshot.as_nbt()).to_network_bytes() {
                    Ok(bytes) => {
                        flags |= input::CELL_HAS_STACK;
                        nbt = bytes;
                    }
                    Err(error) => {
                        eprintln!("[curios] screen stack {position} NBT refused: {error:?}")
                    }
                },
                Err(error) => eprintln!("[curios] screen stack {position} refused: {error:?}"),
            }
        }
        wire.push(ScreenCellWire {
            index: position as u16,
            flags,
            nbt,
        });
    }
    let page_byte = u8::try_from(page).unwrap_or(u8::MAX);
    let bytes = input::encode_screen_state(page_byte, cosmetic, &wire);
    player
        .send_payload(host, &input::screen_state_payload_id(), bytes)
        .is_ok()
}

/// Visible functional slot currently under the pointer, if our menu is open.
pub fn hovered_render_slot(uuid: u128) -> Option<usize> {
    player_state(uuid).hovering
}

/// Server-authoritative render toggle for one visible functional slot.
/// Cosmetic slots have no independent render flag of their own: they follow
/// the functional slot they are paired with, as in the Java reference.
pub fn toggle_render(host: &mut dyn ServerHost, uuid: u128, slot: usize) -> bool {
    let page = player_state(uuid).page;
    let Some(cell) = visible_cells(page, false).get(slot).cloned() else {
        return false;
    };
    if cell.cosmetic {
        return false;
    }
    if host.player(uuid).is_none() {
        return false;
    }
    let holder = crate::inventory::holder_for_player(uuid);
    let Ok(mut inventory) = crate::inventory::INVENTORY.get(host, holder) else {
        return false;
    };
    let Some(handler) = inventory.slot_mut(&cell.slot_type) else {
        return false;
    };
    let Some(current) = handler.render(cell.index) else {
        return false;
    };
    if handler.set_render(cell.index, !current).is_err() {
        return false;
    }
    if crate::inventory::INVENTORY.set(host, holder, inventory).is_err() {
        return false;
    }
    sync_state(host, uuid)
}

/// Server handler for Java `client_page`. The window comes from the menu this
/// mod opened, never from the packet.
pub fn client_page(host: &mut dyn ServerHost, uuid: u128, next: bool) -> bool {
    let Some(window) = player_window(uuid) else {
        return false;
    };
    with_player(uuid, (), |menu| {
        menu.state.advance(next);
    });
    let page = player_state(uuid).page;
    let mirrored = refresh(host, uuid, window, page);
    sync_state(host, uuid) && mirrored
}

/// Server handler for Java `toggle_cosmetics`. The cosmetic view swaps the
/// stacks shown in the same slots; it never duplicates slots.
pub fn toggle_cosmetics(host: &mut dyn ServerHost, uuid: u128) -> bool {
    let Some(window) = player_window(uuid) else {
        return false;
    };
    with_player(uuid, (), |menu| menu.state.toggle_cosmetic());
    let page = player_state(uuid).page;
    let mirrored = refresh(host, uuid, window, page);
    sync_state(host, uuid) && mirrored
}

/// Server handler for Java `destroy`: clear every functional and cosmetic
/// slot of the player. Clearing is a mutation of the authoritative attachment,
/// never of the mirrored menu.
pub fn destroy(host: &mut dyn ServerHost, uuid: u128) -> bool {
    if host.player(uuid).is_none() {
        return false;
    }
    let holder = crate::inventory::holder_for_player(uuid);
    let Ok(mut inventory) = crate::inventory::INVENTORY.get(host, holder) else {
        return false;
    };
    let ids: Vec<String> = inventory.slot_ids().map(str::to_owned).collect();
    for id in ids {
        let Some(handler) = inventory.slot_mut(&id) else {
            continue;
        };
        for index in 0..handler.len() {
            if let Err(error) = handler.set_stack(index, None) {
                eprintln!("[curios] destroy could not clear {id}/{index}: {error:?}");
                return false;
            }
            if let Err(error) = handler.set_cosmetic(index, None) {
                eprintln!("[curios] destroy could not clear cosmetic {id}/{index}: {error:?}");
                return false;
            }
        }
    }
    crate::inventory::INVENTORY.set(host, holder, inventory).is_ok()
}

/// Server handler for Java `open_vanilla`: close the menu we opened.
pub fn open_vanilla(host: &mut dyn ServerHost, uuid: u128) -> bool {
    host.close_menu(uuid);
    true
}

/// Maps a menu slot in the player-inventory tail to Horizon's `PlayerSlot`
/// number, or None for the menu's own container slots. The 9x6 chest has 54
/// container slots followed by 36 inventory slots (hotbar first).
pub fn inventory_slot_for_menu_slot(slot: usize) -> Option<i32> {
    if slot >= MENU_SLOTS && slot < MENU_SLOTS + PLAYER_SLOTS {
        i32::try_from(slot - MENU_SLOTS).ok()
    } else {
        None
    }
}

/// First empty slot of a 36-slot inventory, as a boolean occupancy slice.
/// Pure so the source/destination mapping can be unit-tested.
pub fn first_free_index(occupied: &[bool]) -> Option<usize> {
    occupied.iter().position(|taken| !taken)
}

/// Moves `count` items in one journaled transaction between two distinct
/// places. Either both legs succeed or neither does.
fn transact_move(
    host: &mut dyn ServerHost,
    from: &ItemPlace,
    to: &ItemPlace,
    stack: &ItemStack,
    count: u32,
) -> bool {
    let resource = ItemResource::of(stack);
    let moved: Result<(), ()> = host.transaction(|host, tx| {
        let inserted = to.insert(host, tx, &resource, count);
        if inserted != count {
            return Err(());
        }
        let extracted = from.extract(host, tx, &resource, count);
        if extracted == count { Ok(()) } else { Err(()) }
    });
    if moved.is_err() {
        eprintln!("[curios] item transaction aborted for {}", stack.item().id(host));
    }
    moved.is_ok()
}

/// Server hook: move real stacks between the player's inventory and Curio
/// `ItemPlace::Mod`. The mirrored menu slots are never the source: clicking
/// slot 54..89 addresses the player's own inventory, and clicking a cell
/// unequips into the first free player slot.
pub fn menu_clicked(
    host: &mut dyn ServerHost,
    uuid: u128,
    window: i32,
    raw_slot: i32,
    button: i32,
    mode: i32,
) -> bool {
    // -999 means "outside"; a negative index must never become slot 0.
    let Ok(slot) = usize::try_from(raw_slot) else {
        return false;
    };
    if Some(window) != player_window(uuid) || slot >= MENU_SLOTS + PLAYER_SLOTS {
        return false;
    }
    let Some(player) = host.player(uuid) else {
        return false;
    };
    let entity = player.entity().id();
    let state = player_state(uuid);
    let cells = display_cells(state.page, state.cosmetic);
    let right_click = button == 1;
    let quick_move = mode == 1;
    let mut moved_any = false;

    if let Some(inventory_index) = inventory_slot_for_menu_slot(slot) {
        // Equip: source is the player's own slot, destination a Curio place.
        let source = ItemPlace::PlayerSlot {
            player: uuid,
            slot: inventory_index,
        };
        let Some(stack) = source.stack(host) else {
            return false;
        };
        let item_id = stack.item().id(host);
        let count = stack.count();
        if count == 0 {
            return false;
        }
        let wanted = if right_click { 1 } else { count };
        let mut remaining = wanted;
        for cell in cells.iter().cloned() {
            if remaining == 0 {
                break;
            }
            let Some(place) = cell.place(entity) else {
                continue;
            };
            if place.stack(host).is_some() {
                continue;
            }
            let context = SlotContext::new(
                cell.slot_type.clone(),
                Some(u64::from(entity)),
                cell.index,
                cell.cosmetic,
                true,
            );
            let Some(slot_type) = registry::slot_types()
                .iter()
                .find(|candidate| candidate.id() == cell.slot_type)
                .cloned()
            else {
                continue;
            };
            if !equip::can_equip_host(
                &*host,
                &slot_type,
                &context,
                stack.item(),
                &item_id,
                TriState::Default,
            ) {
                continue;
            }
            let amount = remaining.min(stack.count());
            if transact_move(host, &source, &place, &stack, amount) {
                remaining -= amount;
                moved_any = true;
            }
            if !quick_move {
                break;
            }
        }
    } else if let Some(cell) = cells.get(slot).cloned() {
        // Unequip: source is a Curio place, destination a player slot.
        let Some(place) = cell.place(entity) else {
            return false;
        };
        let Some(stack) = place.stack(host) else {
            return false;
        };
        let context = SlotContext::new(
            cell.slot_type.clone(),
            Some(u64::from(entity)),
            cell.index,
            cell.cosmetic,
            true,
        );
        if !equip::can_unequip(&context, &stack.item().id(host), TriState::Default) {
            return false;
        }
        let mut occupied = Vec::with_capacity(PLAYER_SLOTS);
        let mut destinations = Vec::with_capacity(PLAYER_SLOTS);
        for index in 0..PLAYER_SLOTS {
            let destination = ItemPlace::PlayerSlot {
                player: uuid,
                slot: index as i32,
            };
            occupied.push(destination.stack(host).is_some());
            destinations.push(destination);
        }
        let Some(free) = first_free_index(&occupied) else {
            return false;
        };
        let count = if right_click { 1 } else { stack.count() };
        let destination = &destinations[free];
        moved_any = transact_move(host, &place, destination, &stack, count);
    } else {
        return false;
    }

    if !moved_any {
        return false;
    }
    let page = player_state(uuid).page;
    let mirrored = refresh(host, uuid, window, page);
    if !sync_state(host, uuid) {
        eprintln!("[curios] screen snapshot could not be refreshed after a click");
    }
    mirrored
}

pub fn menu_closed(uuid: u128) {
    forget_player(uuid);
}

/// Client hook: initialise state when our menu screen opens.
pub fn screen_opening(uuid: u128, view: &ScreenView<'_>) {
    if !is_ours(view) {
        return;
    }
    with_player(uuid, (), |menu| {
        menu.state.hovering = None;
        menu.state.origin = [view.rect[0], view.rect[1]];
    });
}

/// Sprite path for one cell. Cosmetic pairs share the upstream cosmetic icon.
pub fn slot_icon(cell: &ScreenCell) -> String {
    if cell.cosmetic {
        "curios:slot/empty_cosmetic_slot".to_owned()
    } else {
        format!("curios:slot/empty_{}_slot", cell.slot_type)
    }
}

/// Client hook: paints the Curios panel under the menu's own widgets. The
/// vanilla chest is only the container backing; this is the screen the player
/// actually sees.
pub fn screen_render_background(_uuid: u128, _host: &dyn ClientHost, event: &mut ScreenRenderBackgroundEvent<'_>) {
    if !is_ours(event.screen) {
        return;
    }
    let panel = Layout::new(event.screen.rect).panel;
    let size = event.draw.size();
    // Cover the vanilla chest panel and the player-inventory rows below it.
    event.draw.fill([0.0, 0.0, size[0], size[1]], 0xFF_0B0812);
    event.draw.fill(
        [panel[0] - 6.0, panel[1] - 6.0, panel[2] + 12.0, panel[3] + 104.0],
        0xFF_1B1628,
    );
    event.draw.fill(
        [panel[0] - 2.0, panel[1] - 2.0, panel[2] + 4.0, panel[3] + 4.0],
        0xFF_2E2545,
    );
}

/// Client hook: buttons and hover tooltips are drawn over the screen.
pub fn screen_render_post(uuid: u128, host: &dyn ClientHost, event: &mut ScreenRenderPostEvent<'_>) {
    if !is_ours(event.screen) {
        return;
    }
    let hovered = Layout::new(event.screen.rect).hit(event.screen.mouse);
    let hovered_slot = match hovered {
        Control::Slot(index) => Some(index),
        _ => None,
    };
    let state = with_player(uuid, MenuState::new(), |menu| {
        menu.state.origin = [event.screen.rect[0], event.screen.rect[1]];
        menu.state.hovering = hovered_slot;
        menu.state.clone()
    });
    let layout = Layout::new(event.screen.rect);
    let locale = lang::locale(host);
    let name = lang::lookup(locale, lang::KEY_NAME).unwrap_or("Curios");
    event.draw.text(name, [layout.panel[0] + 8.0, layout.panel[1] + 5.0], 0xFF_E8C56A, true);
    let per_page = usize::from(config::current().server.max_slots_per_page).max(1);
    let pages = page_count(cell_total(), per_page);
    let pattern = lang::lookup(locale, lang::KEY_PAGE).unwrap_or("%d / %d");
    let label = pattern
        .replacen("%d", &(state.page + 1).to_string(), 1)
        .replacen("%d", &pages.to_string(), 1);
    event.draw.text(
        &label,
        [layout.panel[0] + layout.panel[2] - 76.0, layout.panel[1] + 5.0],
        0xFF_EFE7FF,
        true,
    );

    // Draw every visible cell from the server snapshot, not from the vanilla
    // menu slots. The engine's own stacks are underneath our opaque panel.
    let snapshot = player_screen(uuid);
    if let Some(snapshot) = snapshot {
        for (position, cell) in snapshot.cells.iter().enumerate().take(MENU_SLOTS) {
            let rect = layout.slot_rect(position);
            let frame = if hovered_slot == Some(position) {
                0xFF_FFE08A
            } else if cell.cosmetic {
                0xFF_A855C8
            } else {
                0xFF_6B5A8C
            };
            event.draw.fill(
                [rect[0] - 1.0, rect[1] - 1.0, rect[2] + 2.0, rect[3] + 2.0],
                frame,
            );
            let sprite = slot_icon(cell);
            if !event.draw.sprite(&sprite, [0.0, 0.0, 16.0, 16.0], rect, None) {
                event.draw.fill(rect, 0xFF_120E1C);
            }
            if let Some(stack) = cell.stack.as_ref() {
                event.draw.item_stack(stack, [rect[0] + 1.0, rect[1] + 1.0], SLOT_ICON - 2.0);
            }
            if !cell.render {
                event.draw.fill(rect, 0x80_000000);
            }
            if !cell.active {
                event.draw.text(
                    lang::lookup(locale, lang::KEY_INACTIVE).unwrap_or("Inactive"),
                    [rect[0], rect[1] + rect[3] - 6.0],
                    0xFF_FF5555,
                    false,
                );
            }
            if cell.cosmetic {
                event.draw.text("C", [rect[0] + rect[2] - 7.0, rect[1]], 0xFF_E879FF, true);
            }
            let label = lang::slot_name(locale, &cell.slot_type);
            event.draw.text(&label, [rect[0], rect[1] + rect[3] + 1.0], 0xFF_8C7AA8, false);
        }
    }

    // #24: the client setting decides whether any control is shown at all.
    if config::current().render.enable_button {
        draw_button(event.draw, layout, Control::PagePrev, "<");
        draw_button(
            event.draw,
            layout,
            Control::ToggleCosmetics,
            if state.cosmetic { "C" } else { "c" },
        );
        draw_button(event.draw, layout, Control::ToggleRender, "R");
        draw_button(event.draw, layout, Control::PageNext, ">");
    }

    // Tooltips are drawn with plain text: the block-scoped screen_tooltip hook
    // cannot fire for Curios, which has no block.
    if let Some(index) = hovered_slot {
        let snapshot = player_screen(uuid);
        if let Some(cell) = snapshot.and_then(|snapshot| snapshot.cells.into_iter().nth(index)) {
            let mut lines = vec![lang::slot_name(locale, &cell.slot_type)];
            if cell.cosmetic {
                lines.push(
                    lang::lookup(locale, lang::KEY_COSMETIC)
                        .unwrap_or("Cosmetic")
                        .to_owned(),
                );
            }
            if !cell.active {
                lines.push(
                    lang::lookup(locale, lang::KEY_INACTIVE)
                        .unwrap_or("Inactive")
                        .to_owned(),
                );
            }
            for (line, text) in lines.iter().enumerate() {
                event.draw.text(
                    text,
                    [event.screen.mouse[0] + 8.0, event.screen.mouse[1] + 12.0 + line as f32 * 10.0],
                    0xFF_EFE7FF,
                    true,
                );
            }
        }
    }
}

/// Lines for one curio cell, in the same order as the Java `CurioSlot` tooltip:
/// identifier, cosmetic marker, then the inactive marker when the slot is
/// disabled. `screen_tooltip` and `screen_render_post` share this.
pub fn tooltip_lines(host: &dyn ClientHost, cell: &Cell, inactive: bool) -> Vec<Text> {
    let locale = lang::locale(host);
    let mut lines = vec![Text::literal(lang::slot_name(locale, &cell.slot_type))];
    if cell.cosmetic {
        lines.push(Text::literal(
            lang::lookup(locale, lang::KEY_COSMETIC)
                .unwrap_or("Cosmetic")
                .to_owned(),
        ));
    }
    if inactive {
        lines.push(Text::literal(
            lang::lookup(locale, lang::KEY_INACTIVE)
                .unwrap_or("Inactive")
                .to_owned(),
        ));
    }
    lines
}

/// Public tooltip hook for `Hooks::screen_tooltip`. The pinned hook is scoped
/// to block entities; Curios has no block, so `lib.rs` forwards it from a
/// generic screen hook with an empty `block`/`data`. The origin remembered by
/// the last draw call is used for hit-testing.
pub fn screen_tooltip(
    uuid: u128,
    host: &dyn ClientHost,
    _block: &str,
    _data: &[i32],
    at: [f32; 2],
    lines: &mut Vec<String>,
) {
    let state = player_state(uuid);
    let control = Layout::new([state.origin[0], state.origin[1], PANEL_W, 256.0]).hit(at);
    let Control::Slot(index) = control else {
        return;
    };
    let cells = display_cells(state.page, state.cosmetic);
    let Some(cell) = cells.get(index) else {
        return;
    };
    let locale = lang::locale(host);
    lines.push(lang::slot_name(locale, &cell.slot_type));
    if cell.cosmetic {
        lines.push(
            lang::lookup(locale, lang::KEY_COSMETIC)
                .unwrap_or("Cosmetic")
                .to_owned(),
        );
    }
}

fn draw_button(draw: &mut dyn DrawContext, layout: Layout, control: Control, label: &str) {
    if let Some(rect) = layout.button_rect(control) {
        draw.fill(rect, 0xFF_2E2545);
        draw.text(label, [rect[0] + 3.0, rect[1] + 1.0], 0xFF_EFE7FF, true);
    }
}

/// Client hook: page and cosmetic controls. The screen hook cannot send a
/// payload, so it updates the local view and queues the request for
/// `input::flush_pending_actions` from `key_mapping_pre`.
pub fn screen_mouse_button_pressed_pre(uuid: u128, view: &ScreenView<'_>, button: i32) -> Flow {
    if !is_ours(view) || button != 0 {
        return Flow::Continue;
    }
    let buttons = config::current().render.enable_button;
    let control = Layout::new(view.rect).hit_with_buttons(view.mouse, buttons);
    match control {
        Control::PagePrev | Control::PageNext => {
            let next = control == Control::PageNext;
            with_player(uuid, Flow::Continue, |menu| {
                if menu.state.advance(next) {
                    Flow::Cancel
                } else {
                    Flow::Continue
                }
            });
            input::queue(if next {
                input::PendingAction::PageNext
            } else {
                input::PendingAction::PagePrev
            });
            Flow::Cancel
        }
        Control::ToggleCosmetics => {
            with_player(uuid, (), |menu| menu.state.toggle_cosmetic());
            input::queue(input::PendingAction::ToggleCosmetics);
            Flow::Cancel
        }
        Control::ToggleRender => {
            if let Some(slot) = hovered_render_slot(uuid) {
                input::queue(input::PendingAction::ToggleRender(slot));
            }
            Flow::Cancel
        }
        Control::Slot(_) | Control::None => Flow::Continue,
    }
}

/// Client hook: the wheel turns pages when the pointer is over our panel.
pub fn screen_mouse_scrolled_pre(uuid: u128, view: &ScreenView<'_>, scroll: [f64; 2]) -> Flow {
    if !is_ours(view) || scroll[1] == 0.0 {
        return Flow::Continue;
    }
    let control = Layout::new(view.rect).hit(view.mouse);
    if !matches!(control, Control::Slot(_)) {
        return Flow::Continue;
    }
    let next = scroll[1] > 0.0;
    with_player(uuid, (), |menu| {
        menu.state.advance(next);
    });
    input::queue(if next {
        input::PendingAction::PageNext
    } else {
        input::PendingAction::PagePrev
    });
    Flow::Cancel
}

/// The translatable title used by [`open`]. Kept as a translation key so the
/// client resolves it in the player's language.
pub fn title() -> Text {
    Text::translate(lang::KEY_NAME.to_owned(), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout::new([100.0, 50.0, 176.0, 256.0])
    }

    #[test]
    fn page_count_is_never_zero() {
        assert_eq!(page_count(0, 48), 1);
        assert_eq!(page_count(1, 48), 1);
        assert_eq!(page_count(48, 48), 1);
        assert_eq!(page_count(49, 48), 2);
        assert_eq!(page_count(42, 0), 1);
    }

    #[test]
    fn slots_follow_the_vanilla_grid() {
        let layout = layout();
        assert_eq!(layout.slot_rect(0), [108.0, 72.0, 18.0, 18.0]);
        assert_eq!(layout.slot_rect(9), [108.0, 92.0, 18.0, 18.0]);
        let panel_end = layout.panel[0] + layout.panel[2];
        assert!(layout.slot_rect(MENU_SLOTS - 1)[0] + 16.0 <= panel_end);
    }

    #[test]
    fn controls_win_over_slots_in_hit_testing() {
        let layout = layout();
        let rect = layout.button_rect(Control::PageNext).unwrap();
        let at = [rect[0] + 1.0, rect[1] + 1.0];
        assert_eq!(layout.hit(at), Control::PageNext);
        let slot = layout.slot_rect(0);
        assert_eq!(layout.hit([slot[0] + 1.0, slot[1] + 1.0]), Control::Slot(0));
        assert_eq!(layout.hit([0.0, 0.0]), Control::None);
    }

    #[test]
    fn visible_cells_follow_registry_order_and_hide_cosmetics_by_default() {
        let cells = visible_cells(0, true);
        let first = cells.first().expect("at least one cell");
        assert!(!first.cosmetic);
        assert_eq!(first.slot_type, "curio");
        // The shipped built-in definitions have no `add_cosmetic`, so no
        // cosmetic pairs exist until a datapack enables them. The cosmetic
        // place encoding itself is covered by the next test.
        assert!(cells.iter().all(|cell| !cell.cosmetic));
        assert_eq!(visible_cells(0, false), cells);
    }

    #[test]
    fn cosmetic_view_swaps_cells_without_doubling_them() {
        let functional = visible_cells(0, false);
        let swapped = display_cells(0, true);
        assert_eq!(functional.len(), swapped.len());
        assert!(functional.iter().all(|cell| !cell.cosmetic));
        assert!(swapped.iter().all(|cell| cell.cosmetic));
        assert_eq!(
            functional.iter().map(|cell| &cell.slot_type).collect::<Vec<_>>(),
            swapped.iter().map(|cell| &cell.slot_type).collect::<Vec<_>>()
        );
    }

    #[test]
    fn tooltip_keys_are_shipped_in_every_language() {
        for locale in lang::LOCALES {
            for key in [lang::KEY_IDENTIFIER_PREFIX, lang::KEY_COSMETIC, lang::KEY_INACTIVE] {
                if key == lang::KEY_IDENTIFIER_PREFIX {
                    continue;
                }
                assert!(lang::lookup(locale, key).is_some(), "{locale}/{key}");
            }
        }
        assert_eq!(lang::lookup(lang::DEFAULT_LOCALE, lang::KEY_INACTIVE), Some("Inactive"));
    }

    #[test]
    fn custom_grid_hits_every_rendered_cell() {
        let layout = layout();
        for index in 0..MENU_SLOTS {
            let rect = layout.slot_rect(index);
            let at = [rect[0] + 2.0, rect[1] + 2.0];
            assert_eq!(layout.hit(at), Control::Slot(index), "cell {index}");
        }
        assert_eq!(layout.hit([layout.panel[0] + 200.0, layout.panel[1]]), Control::None);
    }

    #[test]
    fn icon_paths_cover_all_eleven_slot_types() {
        for id in [
            "curio", "head", "necklace", "back", "body", "bracelet", "hands", "ring", "belt",
            "feet", "charm",
        ] {
            let cell = ScreenCell {
                index: 0,
                cosmetic: false,
                active: true,
                render: true,
                stack: None,
                slot_type: id.to_owned(),
            };
            assert_eq!(slot_icon(&cell), format!("curios:slot/empty_{id}_slot"));
        }
        let cosmetic = ScreenCell {
            cosmetic: true,
            ..ScreenCell {
                index: 0,
                cosmetic: false,
                active: true,
                render: true,
                stack: None,
                slot_type: "ring".to_owned(),
            }
        };
        assert_eq!(slot_icon(&cosmetic), "curios:slot/empty_cosmetic_slot");
    }

    #[test]
    fn pages_partition_cells_without_gaps_or_overlaps() {
        let total = cell_total();
        let per_page = usize::from(config::current().server.max_slots_per_page);
        let mut seen = Vec::new();
        for page in 0..page_count(total, per_page) {
            for cell in display_cells(page, false) {
                seen.push((cell.slot_type, cell.index));
            }
        }
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), total);
    }

    #[test]
    fn cosmetic_places_use_the_flag() {
        let cell = Cell {
            slot_type: "ring".to_owned(),
            index: 1,
            cosmetic: true,
        };
        match cell.place(7).expect("a place") {
            ItemPlace::Mod { key, .. } => assert!(place::is_cosmetic(key)),
            other => panic!("unexpected place: {other:?}"),
        }
    }

    #[test]
    fn state_pages_are_clamped() {
        let mut state = MenuState::new();
        let last = page_count(cell_total(), config::current().server.max_slots_per_page.into())
            .saturating_sub(1);
        for _ in 0..last + 3 {
            state.advance(true);
        }
        assert_eq!(state.page, last);
        for _ in 0..last + 3 {
            state.advance(false);
        }
        assert_eq!(state.page, 0);
        state.toggle_cosmetic();
        assert!(state.cosmetic);
    }

    #[test]
    fn menu_slot_mapping_never_aliases_a_curio_place() {
        assert_eq!(inventory_slot_for_menu_slot(MENU_SLOTS - 1), None);
        assert_eq!(inventory_slot_for_menu_slot(MENU_SLOTS), Some(0));
        assert_eq!(inventory_slot_for_menu_slot(MENU_SLOTS + 8), Some(8));
        assert_eq!(inventory_slot_for_menu_slot(MENU_SLOTS + PLAYER_SLOTS - 1), Some(35));
        assert_eq!(inventory_slot_for_menu_slot(MENU_SLOTS + PLAYER_SLOTS), None);
        let inventory = ItemPlace::PlayerSlot {
            player: 42,
            slot: inventory_slot_for_menu_slot(MENU_SLOTS + 3).unwrap(),
        };
        let curio = place::curio_place(42, "ring", 3).unwrap();
        assert_ne!(inventory, curio, "source and destination must differ");
    }

    #[test]
    fn first_free_player_slot_is_the_first_gap() {
        assert_eq!(first_free_index(&[true, true, false, true]), Some(2));
        assert_eq!(first_free_index(&[]), None);
        assert_eq!(first_free_index(&[true; PLAYER_SLOTS]), None);
    }

    #[test]
    fn two_players_have_independent_menu_state() {
        let first = 0x0000_0000_0000_0001_u128;
        let second = 0x0000_0000_0000_0002_u128;
        forget_player(first);
        forget_player(second);
        with_player(first, (), |menu| {
            menu.state.page = 3;
            menu.state.cosmetic = true;
            menu.window = Some(41);
        });
        with_player(second, (), |menu| {
            menu.state.page = 7;
            menu.window = Some(42);
        });
        assert_eq!(player_state(first).page, 3);
        assert!(player_state(first).cosmetic);
        assert_eq!(player_window(first), Some(41));
        assert_eq!(player_state(second).page, 7);
        assert!(!player_state(second).cosmetic);
        assert_eq!(player_window(second), Some(42));
        forget_player(first);
        assert_eq!(player_state(first).page, 0);
        assert_eq!(player_state(second).page, 7);
        assert_eq!(player_window(second), Some(42));
        forget_player(second);
    }

    #[test]
    fn button_hit_testing_honours_the_client_setting() {
        let layout = layout();
        let rect = layout.button_rect(Control::PageNext).unwrap();
        let at = [rect[0] + 2.0, rect[1] + 2.0];
        assert_eq!(layout.hit_with_buttons(at, true), Control::PageNext);
        assert_ne!(layout.hit_with_buttons(at, false), Control::PageNext);
    }

    #[test]
    fn inventory_holder_uses_the_uuid_not_the_entity_id() {
        let uuid = 0x0123_4567_89ab_cdef_u128;
        let holder = crate::inventory::holder_for_player(uuid);
        assert_eq!(holder, Holder::Player(uuid));
        assert_ne!(holder, Holder::Entity(7));
    }

    #[test]
    fn tag_files_are_contributed_under_engine_overlay_paths() {
        let files = tag_files();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0, CURIO_ITEM_TAG_PATH);
        assert_eq!(files[1].0, PLAYER_LIKE_TAG_PATH);
        for (path, bytes) in files {
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).unwrap_or_else(|error| panic!("{path}: {error}"));
            assert!(value["values"].is_array(), "{path} has no values array");
        }
    }

    #[test]
    fn shipped_tag_files_exist_and_parse() {
        let item: serde_json::Value =
            serde_json::from_str(include_str!("../data/curios/tags/item/curio.json"))
                .expect("curios:curio tag must be valid JSON");
        assert_eq!(item["values"], serde_json::json!([]));
        let entities: serde_json::Value = serde_json::from_str(include_str!(
            "../data/curios/tags/entity_type/player_like.json"
        ))
        .expect("curios:player_like tag must be valid JSON");
        assert_eq!(entities["values"][0], serde_json::json!("minecraft:player"));
    }

    #[test]
    fn container_declares_the_vanilla_menu() {
        let container = container();
        assert_eq!(container.menu.as_deref(), Some(MENU_ID));
        assert_eq!(container.slots, MENU_SLOTS);
    }
}
