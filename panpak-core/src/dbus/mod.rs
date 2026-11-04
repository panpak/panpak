// SPDX-License-Identifier: MIT OR Apache-2.0
//! A small D-Bus client for the outer stub: wire format and a blocking connection.
//! See `wire` for the limits that make the reader safe on input that is not trusted.

mod conn;
mod proxy;
mod rules;
mod systemd;
mod wire;

pub use conn::{address_path, Connection, DbusError, Reply};
pub use proxy::{find_proxy, parse_version, proxy_status, proxy_version, session_bus_path, ProxyStatus, MIN_PROXY, PROXY_PATHS};
pub use rules::{check_dbus_own, id_own_problem, own_name_problem, proxy_args, tray_names, BusNeeds, MAX_OWN, PORTAL, TRAY_WATCHER, VIEW_APP_PID};
pub use systemd::{grant_document, portal_id_from_unit, scope_name, start_scope, DocumentGrant};
pub use wire::{
    check_elem, check_signature, check_single, encode_message, message_len, parse_message, split_signature, valid_dotted_name, valid_object_path, Limits, Message, Reader, Value, Writer, CALL, ERROR, RETURN, SIGNAL,
};
