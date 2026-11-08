// SPDX-License-Identifier: MIT OR Apache-2.0
//! A small D-Bus client for the outer stub: wire format and a blocking connection.
//! See `wire` for the limits that make the reader safe on input that is not trusted.

mod conn;
mod proxy;
mod route;
mod rules;
mod systemd;
mod wire;

pub use conn::{address_path, Connection, DbusError, Reply};
pub use proxy::{find_proxy, parse_version, proxy_status, proxy_status_cached, proxy_version, session_bus_path, ProxyStatus, MIN_PROXY, PROXY_PATHS};
pub use route::{mpris_alias, ClientId, Config as RouterConfig, Msg, Out, Router, MAX_MESSAGE as ROUTER_MAX_MESSAGE, MPRIS_PREFIX, OPENURI_IFACE, REGISTRY_IFACE};
pub use rules::{check_dbus_own, id_own_problem, own_name_problem, proxy_args, BusNeeds, MAX_OWN, PORTAL, TRAY_WATCHER};
pub use systemd::{grant_document, portal_id_from_unit, scope_name, start_scope, DocumentGrant};
pub use wire::{
    check_elem, check_signature, check_single, encode_message, encode_with_body, message_len, parse_head, parse_message, split_signature, valid_dotted_name, valid_object_path, Head, Limits, Message, Reader, Value, Writer, CALL, ERROR, RETURN, SIGNAL,
};
