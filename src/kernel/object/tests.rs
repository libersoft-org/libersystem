use alloc::sync::Arc;
use core::any::Any;

use super::handle::{HandleError, HandleTable};
use super::rights::Rights;
use super::{KernelObject, ObjectHeader, ObjectType};

pub(crate) struct TestObject {
	header: ObjectHeader,
	value: u64,
}

impl TestObject {
	pub(crate) fn new(value: u64) -> Arc<Self> {
		Arc::new(Self { header: ObjectHeader::new(), value })
	}

	fn value(&self) -> u64 {
		self.value
	}
}

impl KernelObject for TestObject {
	fn header(&self) -> &ObjectHeader {
		&self.header
	}

	fn object_type(&self) -> ObjectType {
		ObjectType::Event
	}

	fn as_any(&self) -> &dyn Any {
		self
	}

	fn into_any_arc(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
		self
	}
}

crate::tagged_test!(handle_create_lookup_close, [Handle, Object, Kernel, Smoke]);
fn handle_create_lookup_close() {
	let mut table = HandleTable::new();
	let obj = TestObject::new(42);
	let handle = table.insert_object(obj, Rights::READ | Rights::WRITE, 0);
	assert_eq!(table.len(), 1);
	let looked = table.lookup(handle, Rights::READ).expect("lookup");
	assert_eq!(looked.as_any().downcast_ref::<TestObject>().unwrap().value(), 42);
	table.close(handle).expect("close");
	assert_eq!(table.len(), 0);
	assert!(matches!(table.lookup(handle, Rights::READ), Err(HandleError::BadHandle)));
}

crate::tagged_test!(handle_rights_enforced, [Handle, Object, Kernel]);
fn handle_rights_enforced() {
	let mut table = HandleTable::new();
	let handle = table.insert_object(TestObject::new(7), Rights::READ, 0);
	assert!(table.lookup(handle, Rights::READ).is_ok());
	assert!(matches!(table.lookup(handle, Rights::WRITE), Err(HandleError::AccessDenied)));
}

crate::tagged_test!(handle_duplicate_attenuates, [Handle, Object, Kernel]);
fn handle_duplicate_attenuates() {
	let mut table = HandleTable::new();
	let handle = table.insert_object(TestObject::new(1), Rights::READ | Rights::WRITE | Rights::DUPLICATE, 0);
	let weak = table.duplicate(handle, Rights::READ).expect("duplicate");
	assert!(table.lookup(weak, Rights::READ).is_ok());
	assert!(matches!(table.lookup(weak, Rights::WRITE), Err(HandleError::AccessDenied)));
	assert!(matches!(table.duplicate(handle, Rights::EXECUTE), Err(HandleError::AccessDenied)));
	let plain = table.insert_object(TestObject::new(2), Rights::READ, 0);
	assert!(matches!(table.duplicate(plain, Rights::READ), Err(HandleError::AccessDenied)));
}

crate::tagged_test!(handle_revocation_invalidates, [Handle, Object, Kernel]);
fn handle_revocation_invalidates() {
	let mut table = HandleTable::new();
	let obj = TestObject::new(99);
	let handle = table.insert_object(obj.clone(), Rights::READ, 0);
	assert!(table.lookup(handle, Rights::READ).is_ok());
	obj.header.revoke();
	assert!(matches!(table.lookup(handle, Rights::READ), Err(HandleError::Revoked)));
}

crate::tagged_test!(handle_type_sealing, [Handle, Object, Kernel]);
fn handle_type_sealing() {
	let mut table = HandleTable::new();
	let handle = table.insert_object(TestObject::new(5), Rights::READ, 0);
	assert!(table.lookup_typed(handle, ObjectType::Event, Rights::READ).is_ok());
	assert!(matches!(table.lookup_typed(handle, ObjectType::Channel, Rights::READ), Err(HandleError::WrongType)));
}

crate::tagged_test!(handle_refcount_lifetime, [Handle, Object, Kernel]);
fn handle_refcount_lifetime() {
	let mut table = HandleTable::new();
	let obj = TestObject::new(3);
	assert_eq!(Arc::strong_count(&obj), 1);
	let handle = table.insert_object(obj.clone(), Rights::READ, 0);
	assert_eq!(Arc::strong_count(&obj), 2);
	let looked = table.lookup(handle, Rights::READ).expect("lookup");
	assert_eq!(Arc::strong_count(&obj), 3);
	drop(looked);
	assert_eq!(Arc::strong_count(&obj), 2);
	table.close(handle).expect("close");
	assert_eq!(Arc::strong_count(&obj), 1);
}
