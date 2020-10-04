pub type PlayerId = String;
pub type RoomId = String;
pub type Role = String;

pub fn find_index<T: PartialEq<O>, O>(slice: &[T], elem: &O) -> Option<usize> {
    slice
        .iter()
        .enumerate()
        .find(|(_, x)| *x == elem)
        .map(|(i, _)| i)
}

pub type AsyncResult<T> = std::result::Result<T, AsyncErr>;
pub type AsyncErr = Box<dyn std::error::Error + Send + Sync>;
