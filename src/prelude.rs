use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

pub type Mutable<T> = RefCell<T>;
pub type Shared<T> = Rc<T>;
pub type AsyncShared<T> = Arc<T>;
