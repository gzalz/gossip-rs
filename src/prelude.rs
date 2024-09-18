use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::{Mutex, RwLock};

pub type Mutable<T> = RefCell<T>;
pub type Shared<T> = Rc<T>;
pub type AsyncShared<T> = Arc<T>;
pub type Mutexed<T> = AsyncShared<Mutex<T>>;
pub type RwLocked<T> = AsyncShared<RwLock<T>>;

