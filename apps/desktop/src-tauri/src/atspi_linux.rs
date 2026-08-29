//! Bounded Linux AT-SPI semantic tree and action adapter.

use personal_agent_context::{
    AccessibilityNode, ActiveView, BackendError, NodeAction, NodeHandle, NodeState, Rect,
    RedactedText, SemanticRole, SnapshotGeneration,
};
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::time::Duration;
use zbus::names::OwnedBusName;
use zbus::zvariant::OwnedObjectPath;
use zbus::{Connection, Proxy};

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const COMPONENT: &str = "org.a11y.atspi.Component";
const ACTION: &str = "org.a11y.atspi.Action";
const EDITABLE_TEXT: &str = "org.a11y.atspi.EditableText";
const TEXT: &str = "org.a11y.atspi.Text";
const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";
const REGISTRY: &str = "org.a11y.atspi.Registry";
const MAX_NODES: usize = 500;
const MAX_DEPTH: usize = 14;
const MAX_TEXT_CHARS: usize = 4_096;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AtspiRef {
    bus: String,
    path: String,
}

impl AtspiRef {
    fn from_owned(bus: &OwnedBusName, path: &OwnedObjectPath) -> Self {
        Self {
            bus: bus.to_string(),
            path: path.to_string(),
        }
    }

    fn opaque_id(&self) -> String {
        format!("atspi|{}|{}", self.bus, self.path)
    }

    fn parse(opaque_id: &str) -> Result<Self, BackendError> {
        let mut parts = opaque_id.splitn(3, '|');
        if parts.next() != Some("atspi") {
            return Err(BackendError::InvalidData(
                "target is not an AT-SPI handle".into(),
            ));
        }
        let bus = parts.next().unwrap_or_default();
        let path = parts.next().unwrap_or_default();
        if bus.is_empty() || path.is_empty() || !bus.starts_with(':') || !path.starts_with('/') {
            return Err(BackendError::InvalidData(
                "AT-SPI handle is malformed".into(),
            ));
        }
        Ok(Self {
            bus: bus.into(),
            path: path.into(),
        })
    }
}

#[derive(Clone, Debug)]
struct RawNode {
    reference: AtspiRef,
    parent: Option<AtspiRef>,
    name: String,
    description: Option<String>,
    role: SemanticRole,
    role_name: String,
    states: BTreeSet<NodeState>,
    interfaces: BTreeSet<String>,
    focusable: bool,
    bounds: Option<Rect>,
    value: Option<String>,
    children: Vec<AtspiRef>,
}

fn dbus_error(context: &str, error: impl std::fmt::Display) -> BackendError {
    BackendError::Operation(format!("AT-SPI {context}: {error}"))
}

async fn accessibility_bus() -> Result<Connection, BackendError> {
    let session = Connection::session()
        .await
        .map_err(|error| dbus_error("session bus connection failed", error))?;
    let bus = Proxy::new(&session, "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus")
        .await
        .map_err(|error| dbus_error("bus proxy failed", error))?;
    let address: String = bus
        .call("GetAddress", &())
        .await
        .map_err(|error| dbus_error("address lookup failed", error))?;
    zbus::connection::Builder::address(address.as_str())
        .map_err(|error| dbus_error("address is invalid", error))?
        .build()
        .await
        .map_err(|error| dbus_error("accessibility bus connection failed", error))
}

async fn proxy<'a>(
    connection: &'a Connection,
    reference: &'a AtspiRef,
    interface: &'a str,
) -> Result<Proxy<'a>, BackendError> {
    Proxy::new(
        connection,
        reference.bus.as_str(),
        reference.path.as_str(),
        interface,
    )
    .await
    .map_err(|error| dbus_error("proxy creation failed", error))
}

async fn children(
    connection: &Connection,
    reference: &AtspiRef,
) -> Result<Vec<AtspiRef>, BackendError> {
    let accessible = proxy(connection, reference, ACCESSIBLE).await?;
    let children: Vec<(OwnedBusName, OwnedObjectPath)> = accessible
        .call("GetChildren", &())
        .await
        .map_err(|error| dbus_error("child enumeration failed", error))?;
    Ok(children
        .into_iter()
        .map(|(bus, path)| AtspiRef::from_owned(&bus, &path))
        .collect())
}

async fn property_string(connection: &Connection, reference: &AtspiRef, name: &str) -> String {
    let Ok(accessible) = proxy(connection, reference, ACCESSIBLE).await else {
        return String::new();
    };
    accessible.get_property(name).await.unwrap_or_default()
}

async fn application_pid(connection: &Connection, bus: &str) -> Option<u32> {
    let proxy = Proxy::new(
        connection,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .await
    .ok()?;
    proxy.call("GetConnectionUnixProcessID", &(bus,)).await.ok()
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

async fn select_application(
    connection: &Connection,
    view: &ActiveView,
) -> Result<AtspiRef, BackendError> {
    let root = AtspiRef {
        bus: REGISTRY.into(),
        path: ROOT_PATH.into(),
    };
    let applications = children(connection, &root).await?;
    let view_names = [
        normalized(&view.application_id),
        normalized(&view.application_name),
    ];
    let mut best: Option<(u16, AtspiRef)> = None;
    for application in applications {
        let pid_score = if let Some(expected) = view.process_id {
            if application_pid(connection, &application.bus).await == Some(expected) {
                1_000
            } else {
                0
            }
        } else {
            0
        };
        let name = normalized(&property_string(connection, &application, "Name").await);
        let name_score = view_names
            .iter()
            .filter(|candidate| !candidate.is_empty())
            .map(|candidate| {
                if name == *candidate {
                    300
                } else if name.contains(candidate) || candidate.contains(&name) {
                    150
                } else {
                    0
                }
            })
            .max()
            .unwrap_or_default();
        let score = pid_score + name_score;
        if score > 0 && best.as_ref().is_none_or(|(current, _)| score > *current) {
            best = Some((score, application));
        }
    }
    best.map(|(_, reference)| reference).ok_or_else(|| {
        BackendError::Unavailable(format!(
            "active application {} is not exposed on AT-SPI",
            view.application_name
        ))
    })
}

async fn read_node(
    connection: &Connection,
    reference: AtspiRef,
    parent: Option<AtspiRef>,
) -> Result<RawNode, BackendError> {
    let accessible = proxy(connection, &reference, ACCESSIBLE).await?;
    let name: String = accessible.get_property("Name").await.unwrap_or_default();
    let description: String = accessible
        .get_property("Description")
        .await
        .unwrap_or_default();
    let role_name: String = accessible
        .call("GetRoleName", &())
        .await
        .unwrap_or_else(|_| "unknown".into());
    let state_words: Vec<u32> = accessible.call("GetState", &()).await.unwrap_or_default();
    let interfaces: BTreeSet<String> = accessible
        .call::<_, _, Vec<String>>("GetInterfaces", &())
        .await
        .unwrap_or_default()
        .into_iter()
        .collect();
    let children: Vec<AtspiRef> = accessible
        .call::<_, _, Vec<(OwnedBusName, OwnedObjectPath)>>("GetChildren", &())
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(bus, path)| AtspiRef::from_owned(&bus, &path))
        .collect();
    let role = semantic_role(&role_name);
    let mut states = semantic_states(&state_words);
    let focusable = state_set(&state_words, 11) || state_set(&state_words, 12);
    if role_name.to_ascii_lowercase().contains("password") {
        states.insert(NodeState::Password);
    }
    if interfaces.contains(EDITABLE_TEXT) {
        states.insert(NodeState::Editable);
    }
    let bounds = if interfaces.contains(COMPONENT) {
        match proxy(connection, &reference, COMPONENT).await {
            Ok(component) => component
                .call::<_, _, (i32, i32, i32, i32)>("GetExtents", &(0_u32,))
                .await
                .ok()
                .and_then(extents_rect),
            Err(_) => None,
        }
    } else {
        None
    };
    let value = if !states.contains(&NodeState::Password)
        && interfaces.contains(TEXT)
        && matches!(
            role,
            SemanticRole::TextField | SemanticRole::SearchField | SemanticRole::Terminal
        ) {
        match proxy(connection, &reference, TEXT).await {
            Ok(text) => text
                .call::<_, _, String>(
                    "GetText",
                    &(0_i32, i32::try_from(MAX_TEXT_CHARS).unwrap_or(i32::MAX)),
                )
                .await
                .ok()
                .map(|value| value.chars().take(MAX_TEXT_CHARS).collect()),
            Err(_) => None,
        }
    } else {
        None
    };
    Ok(RawNode {
        reference,
        parent,
        name,
        description: (!description.is_empty()).then_some(description),
        role,
        role_name,
        states,
        interfaces,
        focusable,
        bounds,
        value,
        children,
    })
}

fn extents_rect((x, y, width, height): (i32, i32, i32, i32)) -> Option<Rect> {
    (width >= 0 && height >= 0).then_some(Rect {
        x: f64::from(x),
        y: f64::from(y),
        width: f64::from(width),
        height: f64::from(height),
    })
}

async fn collect_tree(
    connection: &Connection,
    root: AtspiRef,
) -> Result<Vec<RawNode>, BackendError> {
    let mut queue = VecDeque::from([(root, None, 0_usize)]);
    let mut seen = HashSet::new();
    let mut nodes = Vec::new();
    while let Some((reference, parent, depth)) = queue.pop_front() {
        if nodes.len() >= MAX_NODES || depth > MAX_DEPTH || !seen.insert(reference.clone()) {
            continue;
        }
        let Ok(node) = read_node(connection, reference, parent).await else {
            continue;
        };
        for child in &node.children {
            queue.push_back((child.clone(), Some(node.reference.clone()), depth + 1));
        }
        nodes.push(node);
    }
    if nodes.is_empty() {
        Err(BackendError::Unavailable(
            "AT-SPI returned an empty semantic tree".into(),
        ))
    } else {
        Ok(nodes)
    }
}

fn to_accessibility_nodes(
    raw_nodes: Vec<RawNode>,
    view: &ActiveView,
    generation: SnapshotGeneration,
) -> Vec<AccessibilityNode> {
    let included: HashSet<_> = raw_nodes
        .iter()
        .map(|node| node.reference.clone())
        .collect();
    raw_nodes
        .into_iter()
        .map(|node| {
            let handle = node_handle(&node.reference, view, generation);
            let actions = semantic_actions(&node.interfaces, node.focusable, &node.role);
            let parent = node
                .parent
                .filter(|parent| included.contains(parent))
                .map(|parent| node_handle(&parent, view, generation));
            let children = node
                .children
                .into_iter()
                .filter(|child| included.contains(child))
                .map(|child| node_handle(&child, view, generation))
                .collect();
            AccessibilityNode {
                handle,
                role: node.role,
                name: node.name,
                description: node.description,
                value: node.value,
                bounds: node.bounds,
                states: node.states,
                actions,
                parent,
                children,
                properties: BTreeMap::from([
                    ("native_backend".into(), "at_spi".into()),
                    ("native_role".into(), node.role_name),
                ]),
            }
        })
        .collect()
}

fn node_handle(
    reference: &AtspiRef,
    view: &ActiveView,
    generation: SnapshotGeneration,
) -> NodeHandle {
    NodeHandle {
        window_id: view.window_id.clone(),
        generation,
        opaque_id: reference.opaque_id(),
    }
}

/// Read the active application's real AT-SPI tree with strict depth/node/time bounds.
pub(crate) async fn semantic_nodes(
    view: &ActiveView,
    generation: SnapshotGeneration,
) -> Result<Vec<AccessibilityNode>, BackendError> {
    tokio::time::timeout(Duration::from_secs(4), async {
        let connection = accessibility_bus().await?;
        let application = select_application(&connection, view).await?;
        let raw = collect_tree(&connection, application).await?;
        Ok(to_accessibility_nodes(raw, view, generation))
    })
    .await
    .map_err(|_| BackendError::Operation("AT-SPI tree traversal timed out".into()))?
}

/// Return whether an opaque generation-bound node belongs to the AT-SPI adapter.
pub(crate) fn is_atspi_handle(handle: &NodeHandle) -> bool {
    handle.opaque_id.starts_with("atspi|")
}

async fn call_bool(
    reference: &AtspiRef,
    interface: &str,
    method: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) -> Result<(), BackendError> {
    let connection = accessibility_bus().await?;
    let proxy = proxy(&connection, reference, interface).await?;
    let changed: bool = proxy
        .call(method, body)
        .await
        .map_err(|error| dbus_error("action failed", error))?;
    if changed {
        Ok(())
    } else {
        Err(BackendError::Operation(format!(
            "AT-SPI {method} reported no change"
        )))
    }
}

pub(crate) async fn focus(handle: &NodeHandle) -> Result<(), BackendError> {
    call_bool(
        &AtspiRef::parse(&handle.opaque_id)?,
        COMPONENT,
        "GrabFocus",
        &(),
    )
    .await
}

pub(crate) async fn press(handle: &NodeHandle) -> Result<(), BackendError> {
    let reference = AtspiRef::parse(&handle.opaque_id)?;
    let connection = accessibility_bus().await?;
    let proxy = proxy(&connection, &reference, ACTION).await?;
    let actions: Vec<(String, String, String)> = proxy
        .call("GetActions", &())
        .await
        .map_err(|error| dbus_error("action discovery failed", error))?;
    let names: Vec<_> = actions.into_iter().map(|(name, _, _)| name).collect();
    let index = press_action_index(&names).ok_or_else(|| {
        BackendError::Unavailable("AT-SPI control has no unambiguous press action".into())
    })?;
    let changed: bool = proxy
        .call("DoAction", &(index,))
        .await
        .map_err(|error| dbus_error("action failed", error))?;
    if changed {
        Ok(())
    } else {
        Err(BackendError::Operation(
            "AT-SPI DoAction reported no change".into(),
        ))
    }
}

pub(crate) async fn set_text(handle: &NodeHandle, text: &RedactedText) -> Result<(), BackendError> {
    call_bool(
        &AtspiRef::parse(&handle.opaque_id)?,
        EDITABLE_TEXT,
        "SetTextContents",
        &(text.expose(),),
    )
    .await
}

pub(crate) async fn scroll_to(handle: &NodeHandle) -> Result<(), BackendError> {
    call_bool(
        &AtspiRef::parse(&handle.opaque_id)?,
        COMPONENT,
        "ScrollTo",
        &(6_u32,),
    )
    .await
}

fn semantic_actions(
    interfaces: &BTreeSet<String>,
    focusable: bool,
    role: &SemanticRole,
) -> BTreeSet<NodeAction> {
    let mut actions = BTreeSet::new();
    if interfaces.contains(COMPONENT) {
        actions.insert(NodeAction::Scroll);
    }
    if focusable {
        actions.insert(NodeAction::Focus);
    }
    if interfaces.contains(ACTION)
        && matches!(
            role,
            SemanticRole::Button
                | SemanticRole::Link
                | SemanticRole::MenuItem
                | SemanticRole::CheckBox
                | SemanticRole::RadioButton
                | SemanticRole::ComboBox
                | SemanticRole::Tab
        )
    {
        actions.insert(NodeAction::Press);
    }
    if interfaces.contains(EDITABLE_TEXT) {
        actions.extend([NodeAction::SetValue, NodeAction::ReplaceSelection]);
    }
    actions
}

fn press_action_index(names: &[String]) -> Option<i32> {
    const ACTIVATION_NAMES: [&str; 8] = [
        "click", "press", "activate", "open", "jump", "toggle", "invoke", "select",
    ];
    names
        .iter()
        .position(|name| {
            let normalized = name.trim().to_ascii_lowercase();
            ACTIVATION_NAMES.contains(&normalized.as_str())
        })
        .or_else(|| (names.len() == 1).then_some(0))
        .and_then(|index| i32::try_from(index).ok())
}

fn semantic_role(role: &str) -> SemanticRole {
    match role.to_ascii_lowercase().as_str() {
        "application" => SemanticRole::Application,
        "frame" | "window" => SemanticRole::Window,
        "dialog" | "alert" => SemanticRole::Dialog,
        "tool bar" => SemanticRole::Toolbar,
        "menu" | "menu bar" => SemanticRole::Menu,
        "menu item" | "check menu item" | "radio menu item" => SemanticRole::MenuItem,
        "push button" | "button" | "toggle button" => SemanticRole::Button,
        "link" => SemanticRole::Link,
        "check box" => SemanticRole::CheckBox,
        "radio button" => SemanticRole::RadioButton,
        "combo box" => SemanticRole::ComboBox,
        "entry" | "text" | "password text" => SemanticRole::TextField,
        "search box" => SemanticRole::SearchField,
        "label" | "caption" => SemanticRole::StaticText,
        "heading" => SemanticRole::Heading,
        "list" | "list box" => SemanticRole::List,
        "list item" => SemanticRole::ListItem,
        "table" | "tree table" => SemanticRole::Table,
        "table row" => SemanticRole::Row,
        "table cell" => SemanticRole::Cell,
        "page tab" => SemanticRole::Tab,
        "image" | "icon" => SemanticRole::Image,
        "slider" | "spin button" => SemanticRole::Slider,
        "scroll pane" => SemanticRole::ScrollArea,
        "terminal" => SemanticRole::Terminal,
        "document frame" | "document web" | "document text" => SemanticRole::Document,
        "panel" | "filler" | "section" | "form" | "grouping" => SemanticRole::Group,
        _ => SemanticRole::Unknown,
    }
}

fn state_set(words: &[u32], state: usize) -> bool {
    words
        .get(state / 32)
        .is_some_and(|word| word & (1_u32 << (state % 32)) != 0)
}

fn semantic_states(words: &[u32]) -> BTreeSet<NodeState> {
    let mut states = BTreeSet::new();
    for (index, state) in [
        (8, NodeState::Enabled),
        (12, NodeState::Focused),
        (23, NodeState::Selected),
        (4, NodeState::Checked),
        (10, NodeState::Expanded),
        (7, NodeState::Editable),
        (3, NodeState::Busy),
    ] {
        if state_set(words, index) {
            states.insert(state);
        }
    }
    if !state_set(words, 25) {
        states.insert(NodeState::Offscreen);
    }
    states
}

#[cfg(test)]
mod tests {
    use super::*;
    use personal_agent_context::WindowId;

    #[test]
    fn handles_round_trip_and_reject_other_native_ids() {
        let reference = AtspiRef {
            bus: ":1.42".into(),
            path: "/org/a11y/atspi/accessible/9".into(),
        };
        assert_eq!(AtspiRef::parse(&reference.opaque_id()).unwrap(), reference);
        assert!(AtspiRef::parse("active-window").is_err());
        assert!(AtspiRef::parse("atspi|not-a-bus|relative").is_err());
    }

    #[test]
    fn role_state_and_action_mapping_is_semantic() {
        assert_eq!(semantic_role("push button"), SemanticRole::Button);
        assert_eq!(semantic_role("document web"), SemanticRole::Document);
        let states = semantic_states(&[(1 << 7) | (1 << 8) | (1 << 12) | (1 << 25)]);
        assert!(states.contains(&NodeState::Editable));
        assert!(states.contains(&NodeState::Enabled));
        assert!(states.contains(&NodeState::Focused));
        assert!(!states.contains(&NodeState::Offscreen));
        let actions = semantic_actions(
            &[COMPONENT.into(), ACTION.into(), EDITABLE_TEXT.into()]
                .into_iter()
                .collect(),
            true,
            &SemanticRole::Button,
        );
        assert!(actions.contains(&NodeAction::Focus));
        assert!(actions.contains(&NodeAction::Press));
        assert!(actions.contains(&NodeAction::SetValue));
        assert_eq!(
            press_action_index(&["show menu".into(), "click".into()]),
            Some(1)
        );
        assert_eq!(
            press_action_index(&["expand".into(), "collapse".into()]),
            None
        );
    }

    #[test]
    fn converted_tree_keeps_generation_bound_edges_and_passwords_redacted() {
        let view = ActiveView {
            application_id: "app".into(),
            application_name: "App".into(),
            process_id: None,
            window_id: WindowId("window".into()),
            title: "Title".into(),
            bounds: None,
            focused_node: None,
            secure_surface: false,
        };
        let parent_ref = AtspiRef {
            bus: ":1.2".into(),
            path: "/root".into(),
        };
        let child_ref = AtspiRef {
            bus: ":1.2".into(),
            path: "/password".into(),
        };
        let raw = vec![
            RawNode {
                reference: parent_ref.clone(),
                parent: None,
                name: "Form".into(),
                description: None,
                role: SemanticRole::Group,
                role_name: "form".into(),
                states: BTreeSet::new(),
                interfaces: BTreeSet::new(),
                focusable: false,
                bounds: None,
                value: None,
                children: vec![child_ref.clone()],
            },
            RawNode {
                reference: child_ref,
                parent: Some(parent_ref),
                name: "Password".into(),
                description: None,
                role: SemanticRole::TextField,
                role_name: "password text".into(),
                states: [NodeState::Password].into(),
                interfaces: [EDITABLE_TEXT.into()].into(),
                focusable: true,
                bounds: None,
                value: None,
                children: vec![],
            },
        ];
        let generation = SnapshotGeneration {
            epoch: 2,
            sequence: 3,
        };
        let nodes = to_accessibility_nodes(raw, &view, generation);
        assert_eq!(nodes[0].children, vec![nodes[1].handle.clone()]);
        assert_eq!(nodes[1].parent, Some(nodes[0].handle.clone()));
        assert_eq!(nodes[1].value, None);
        assert_eq!(nodes[1].handle.generation, generation);
    }

    #[tokio::test]
    async fn live_personal_agent_tree_when_explicitly_requested() {
        if std::env::var("PERSONAL_AGENT_ATSPI_LIVE_TEST").as_deref() != Ok("1") {
            return;
        }
        let view = ActiveView {
            application_id: "personal-agent-desktop".into(),
            application_name: "Personal Agent".into(),
            process_id: None,
            window_id: WindowId("live-window".into()),
            title: "Personal Agent".into(),
            bounds: None,
            focused_node: None,
            secure_surface: false,
        };
        let generation = SnapshotGeneration {
            epoch: 1,
            sequence: 1,
        };
        let nodes = semantic_nodes(&view, generation)
            .await
            .expect("live Personal Agent AT-SPI tree");
        assert!(nodes.len() > 10, "expected a real semantic tree");
        assert!(nodes.iter().all(
            |node| node.properties.get("native_backend").map(String::as_str) == Some("at_spi")
        ));
        assert!(nodes.iter().any(|node| {
            node.actions.contains(&NodeAction::Press)
                || node.actions.contains(&NodeAction::SetValue)
        }));
    }
}
