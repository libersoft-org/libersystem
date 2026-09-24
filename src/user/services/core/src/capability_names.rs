// THE CAPABILITY NAMES THE BROKER RESOLVES for the services this crate added to the system: one name per
// root, because the root is the authority. They live here and not in `rt` beside the older names because
// `rt` is the runtime every staged library links, and a change to its sources is a new `lsrt` identity -
// which the foreign quarantine artifacts, linked against the runtime as it was and not rebuilt by this tree,
// would then refuse. Every user of these names is in this crate.

// BLUETOOTHSERVICE'S THREE AUTHORITIES, one name per root, because the root is the authority: a
// component granted the read name cannot resolve the operator one. The profile name is InputService's
// role tag, which is what that service re-resolves after the stack is restarted.
pub const CAP_BT_READ: &[u8] = b"BTREAD";
pub const CAP_BT_OPERATOR: &[u8] = b"BTOPERATOR";
pub const CAP_BT_PROFILE: &[u8] = b"BLUETOOTH";
// POWERSERVICE'S TWO ROOTS: normalised state, and the device controls an operator may use. Resolved
// by name like the Bluetooth ones, so PermissionManager holds neither from bring-up and its grants
// survive a restart of the service.
pub const CAP_POWER_STATE: &[u8] = b"POWERSTATE";
pub const CAP_POWER_CONTROL: &[u8] = b"POWERCONTROL";
// SMARTCARDSERVICE'S MINTING ROOT, which PermissionManager alone resolves: every smart-card grant is a
// connection minted from it for one reader and one component.
pub const CAP_SMARTCARD_ADMIN: &[u8] = b"SMARTCARDADMIN";
// MODEMSERVICE'S OBSERVATION ROOT, which a `modem-state` grant is a connection from, and its minting root,
// which PermissionManager alone resolves for the three minted authorities.
pub const CAP_MODEM_STATE: &[u8] = b"MODEMSTATE";
pub const CAP_MODEM_ADMIN: &[u8] = b"MODEMADMIN";
pub const CAP_CAMERA: &[u8] = b"CAMERA";
pub const CAP_CAMERA_ADMIN: &[u8] = b"CAMERAADMIN";
pub const CAP_MIDI: &[u8] = b"MIDI";
pub const CAP_MIDI_ADMIN: &[u8] = b"MIDIADMIN";
// SPOOLSERVICE'S ONE ROOT: every `spool` grant is a fresh connection from it, and that connection is the
// grant context its jobs are charged to.
pub const CAP_SPOOL: &[u8] = b"SPOOL";
// MEDIAIMPORTSERVICE'S ONE ROOT: every `media-import` grant is a fresh, read-only connection from it.
pub const CAP_MEDIA_IMPORT: &[u8] = b"IMPORT";
// ADMINSERVICE'S THREE ROOTS: the request factory PermissionManager alone resolves and mints every
// `admin-request` connection from, the operator's journal view an `admin-audit` grant is a connection from, and
// the development image's test controls.
pub const CAP_ADMIN_FACTORY: &[u8] = b"ADMINFACTORY";
pub const CAP_ADMIN_AUDIT: &[u8] = b"ADMINAUDIT";
pub const CAP_ADMIN_TEST: &[u8] = b"ADMINTEST";
