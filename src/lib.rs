//! This crate provides functions that allow moving files to the operating system's Recycle Bin or
//! Trash, or the equivalent.
//!
//! Furthermore on Linux and on Windows the [`list`], [`purge_all`], and [`restore_all`] functions
//! can be used to list the contents of the trash, remove selected items permanently, and restore
//! selected items from the trash, respectively. Unfortunately MacOS does not seem to provide the
//! necessary APIs or tools to implement these. If you have an idea how these could be implemented
//! on a Mac, please don't hesitate to get involved at https://github.com/ArturKovacs/trash.
//!
//! # Notes on the Linux implementation
//!
//! This library implements version 1.0 of the [Freedesktop.org Trash](https://specifications.freedesktop.org/trash-spec/trashspec-1.0.html)
//! specification and aims to match the behaviour of Ubuntu 18.04 in cases of ambiguity. Most if
//! not all Linux distributions that ship with a desktop environment follow this specification.
//! This crate blindly assumes that the Linux distribution it runs on follows this specification.
//!
//! [`list`]: linux_windows/fn.list.html
//! [`purge_all`]: linux_windows/fn.purge_all.html
//! [`restore_all`]: linux_windows/fn.restore_all.html
//!

use std::collections::HashSet;
use std::ffi::OsString;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use std::fmt;

#[cfg(test)]
mod tests;

#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod platform;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod platform;

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod platform;

/// Error that might happen during a trash operation.
#[derive(Debug)]
pub struct Error {
    source: Option<Box<dyn std::error::Error + 'static>>,
    kind: ErrorKind,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let intro = "Error during a `trash` operation:";
        if let Some(ref source) = self.source {
            write!(f, "{} ( {:?} ) Source was '{}'", intro, self.kind, source)
        } else {
            write!(f, "{} ( {:?} ) Source error is not specified.", intro, self.kind)
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref()?.as_ref())
    }
}
impl Error {
    pub fn new(kind: ErrorKind, source: Box<dyn std::error::Error + 'static>) -> Error {
        Error { source: Some(source), kind }
    }
    pub fn kind_only(kind: ErrorKind) -> Error {
        Error { source: None, kind }
    }
    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }
    pub fn into_kind(self) -> ErrorKind {
        self.kind
    }
    pub fn into_source(self) -> Option<Box<dyn std::error::Error + 'static>> {
        self.source
    }
    /// Returns `Some` if the source is an `std::io::Error` error. Returns `None` otherwise.
    ///
    /// In other words this is a shorthand for
    /// `self.source().map(|x| x.downcast_ref::<std::io::Error>())`
    pub fn io_error_source(&self) -> Option<&std::io::Error> {
        self.source.as_ref()?.downcast_ref::<std::io::Error>()
    }
}

///
/// A type that is contained within [`Error`]. It provides information about why the error was
/// produced. Some `ErrorKind` variants may promise that calling `source()`
/// (from `std::error::Error`) on [`Error`] will return a reference to a certain type of
/// `std::error::Error`.
///
/// For example further information can be extracted from a `CanonicalizePath` error as shown
/// below.
///
/// ```rust
/// use std::error::Error;
/// let result = trash::remove_all(&["non-existing"]);
/// if let Err(err) = result {
///     match err.kind() {
///         trash::ErrorKind::CanonicalizePath{..} => (), // This is what we expect
///         _ => panic!()
///     };
///     // Long format
///     let io_kind = err.source().unwrap().downcast_ref::<std::io::Error>().unwrap().kind();
///     assert_eq!(io_kind, std::io::ErrorKind::NotFound);
///     // Short format
///     let io_kind = err.io_error_source().unwrap().kind();
///     assert_eq!(io_kind, std::io::ErrorKind::NotFound);
/// }
/// ```
///
/// [`Error`]: struct.Error.html
#[derive(Debug)]
pub enum ErrorKind {
    /// Any error that might happen during a direct call to a platform specific API.
    ///
    /// `function_name`: the name of the function during which the error occured.
    /// `code`: An error code that the function provided or was obtained otherwise.
    ///
    /// On Windows the `code` will contain the HRESULT that the function returned or that was
    /// obtained with `HRESULT_FROM_WIN32(GetLastError())`
    PlatformApi { function_name: &'static str, code: Option<i32> },

    /// Error while canonicalizing path.
    ///
    /// The `source()` function of the `Error` will return a reference to an `std::io::Error`.
    CanonicalizePath {
        /// Path that triggered the error.
        original: PathBuf,
    },

    /// Error while converting an OsString to a String.
    ///
    /// This error kind will not provide a `source()` but it directly corresponds to the error
    /// returned by https://doc.rust-lang.org/std/ffi/struct.OsString.html#method.into_string
    ConvertOsString {
        /// The string that was attempted to be converted.
        original: OsString,
    },

    /// Signals an error that occured during some operation on a file or folder.
    ///
    /// In some cases the `source()` function of the `Error` will return a reference to an
    /// `std::io::Error` but this is not guaranteed.
    ///
    /// `path`: The path to the file or folder on which this error occured.
    Filesystem { path: PathBuf },

    /// This kind of error happens when a trash item's original parent already contains an item with
    /// the same name and type (file or folder). In this case an error is produced and the
    /// restoration of the files is halted meaning that there may be files that could be restored
    /// but were left in the trash due to the error.
    ///
    /// One should not assume any relationship between the order that the items were supplied and
    /// the list of remaining items. That is to say, it may be that the item that collided was in
    /// the middle of the provided list but the remaining items' list contains all the provided
    /// items.
    ///
    /// `path`: The path of the file that's blocking the trash item from being restored.
    /// `remaining_items`: All items that were not restored in the order they were provided,
    /// starting with the item that triggered the error.
    RestoreCollision { path: PathBuf, remaining_items: Vec<TrashItem> },

    /// This sort of error is returned when multiple items with the same `original_path` were
    /// requested to be restored. These items are referred to as twins here.
    ///
    /// `path`: The `original_path` of the twins.
    /// `items`: The complete list of items that were handed over to the `restore_all` function.
    RestoreTwins { path: PathBuf, items: Vec<TrashItem> },
}

/// This struct holds information about a single item within the trash.
///
/// Some functions associated with this struct are defined in the `TrahsItemPlatformDep` trait.
/// That trait is implemented for `TrashItem` by each platform specific source file individually.
///
/// A trahs item can be a file or folder or any other object that the target operating system
/// allows to put into the trash.
#[derive(Debug)]
pub struct TrashItem {
    /// A system specific identifier of the item in the trash.
    ///
    /// On Windows it is the string returned by `IShellFolder::GetDisplayNameOf` with the
    /// `SHGDN_FORPARSING` flag.
    ///
    /// On Linux it is an absolute path to the `.trashinfo` file associated with the item.
    pub id: OsString,

    /// The name of the item. For example if the folder '/home/user/New Folder' was deleted,
    /// its `name` is 'New Folder'
    pub name: String,

    /// The path to the parent folder of this item before it was put inside the trash.
    /// For example if the folder '/home/user/New Folder' is in the trash, its `original_parent`
    /// is '/home/user'.
    ///
    /// To get the full path to the file in its original location use the `original_path`
    /// function.
    pub original_parent: PathBuf,

    /// The date and time in UNIX Epoch time when the item was put into the trash.
    pub time_deleted: i64,
}
/// Platform independent functions of `TrashItem`.
///
/// See `TrahsItemPlatformDep` for platform dependent functions.
impl TrashItem {
    /// Joins the `original_parent` and `name` fields to obtain the full path to the original file.
    pub fn original_path(&self) -> PathBuf {
        self.original_parent.join(&self.name)
    }
}
impl PartialEq for TrashItem {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for TrashItem {}
impl Hash for TrashItem {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// Removes a single file or directory.
///
/// # Example
///
/// ```
/// extern crate trash;
/// use std::fs::File;
/// use trash::remove;
/// File::create("remove_me").unwrap();
/// trash::remove("remove_me").unwrap();
/// assert!(File::open("remove_me").is_err());
/// ```
pub fn remove<T: AsRef<Path>>(path: T) -> Result<(), Error> {
    platform::remove(path)
}

/// Removes all files/directories specified by the collection of paths provided as an argument.
///
/// # Example
///
/// ```
/// extern crate trash;
/// use std::fs::File;
/// use trash::remove_all;
/// File::create("remove_me_1").unwrap();
/// File::create("remove_me_2").unwrap();
/// remove_all(&["remove_me_1", "remove_me_2"]).unwrap();
/// assert!(File::open("remove_me_1").is_err());
/// assert!(File::open("remove_me_2").is_err());
/// ```
pub fn remove_all<I, T>(paths: I) -> Result<(), Error>
where
    I: IntoIterator<Item = T>,
    T: AsRef<Path>,
{
    platform::remove_all(paths)
}

pub use linux_windows::*;

#[cfg(any(target_os = "linux", target_os = "windows"))]
mod linux_windows {
    //!
    //! This module contains functions that are only available on Windows and Linux.
    //! See the [crate description] for more.
    //!
    //! [crate description]: ../index.html
    //!

    use super::*;

    /// Returns all [`TrashItem`]s that are currently in the trash.
    ///
    /// The items are in no particular order and must be sorted when any kind of ordering is required.
    ///
    /// # Example
    ///
    /// ```
    /// use trash::{remove, linux_windows::list};
    /// let trash_items = list().unwrap();
    /// println!("{:#?}", trash_items);
    /// ```
    ///
    /// [`TrashItem`]: ../struct.TrashItem.html
    pub fn list() -> Result<Vec<TrashItem>, Error> {
        platform::list()
    }

    /// Deletes all the provided [`TrashItem`]s permanently.
    ///
    /// This function consumes the provided items.
    ///
    /// # Example
    ///
    /// ```
    /// use std::fs::File;
    /// use trash::{remove, linux_windows::{list, purge_all}};
    /// let filename = "trash-purge_all-example";
    /// File::create(filename).unwrap();
    /// remove(filename).unwrap();
    /// // Collect the filtered list just so that we can make sure there's exactly one element.
    /// // There's no need to `collect` it otherwise.
    /// let selected: Vec<_> = list().unwrap().into_iter().filter(|x| x.name == filename).collect();
    /// assert_eq!(selected.len(), 1);
    /// purge_all(selected).unwrap();
    /// ```
    ///
    /// [`TrashItem`]: ../struct.TrashItem.html
    pub fn purge_all<I>(items: I) -> Result<(), Error>
    where
        I: IntoIterator<Item = TrashItem>,
    {
        platform::purge_all(items)
    }

    /// Restores all the provided [`TrashItem`] to their original location.
    ///
    /// This function consumes the provided items.
    ///
    /// # Errors
    ///
    /// It may be the case that when restoring a file or a folder, the `original_path` already has
    /// a new item with the same name. When such a collision happens this function returns a
    /// [`RestoreCollision`] kind of error.
    ///
    /// If two or more of the provided items have identical `original_path`s then a
    /// [`RestoreTwins`] kind of error is returned.
    ///
    /// # Example
    ///
    /// ```
    /// use std::fs::File;
    /// use trash::{remove, linux_windows::{list, restore_all}};
    /// let filename = "trash-restore_all-example";
    /// File::create(filename).unwrap();
    /// restore_all(list().unwrap().into_iter().filter(|x| x.name == filename)).unwrap();
    /// std::fs::remove_file(filename).unwrap();
    /// ```
    ///
    /// [`RestoreCollision`]: ../enum.ErrorKind.html#variant.RestoreCollision
    /// [`RestoreTwins`]: ../enum.ErrorKind.html#variant.RestoreTwins
    pub fn restore_all<I>(items: I) -> Result<(), Error>
    where
        I: IntoIterator<Item = TrashItem>,
    {
        // Check for twins here cause that's pretty platform independent.
        struct ItemWrapper<'a>(&'a TrashItem);
        impl<'a> PartialEq for ItemWrapper<'a> {
            fn eq(&self, other: &Self) -> bool {
                self.0.original_path() == other.0.original_path()
            }
        }
        impl<'a> Eq for ItemWrapper<'a> {}
        impl<'a> Hash for ItemWrapper<'a> {
            fn hash<H: Hasher>(&self, state: &mut H) {
                self.0.original_path().hash(state);
            }
        }
        let items = items.into_iter().collect::<Vec<_>>();
        let mut item_set = HashSet::with_capacity(items.len());
        for item in items.iter() {
            if !item_set.insert(ItemWrapper(item)) {
                return Err(Error::kind_only(ErrorKind::RestoreTwins {
                    path: item.original_path(),
                    items: items,
                }));
            }
        }
        platform::restore_all(items)
    }
}
