/// The platform's version.
///
/// It travels in two places that have to agree: the ABI section of the module (so a consumer can
/// fold it into a cache key) and the package a binary offers (so two binaries that ship different
/// platforms are not mistaken for each other). A change to any Java under `java/` changes the
/// bytes, which changes the key; the number exists for the changes bytes cannot show, and is bumped
/// by hand when the platform's *contract* changes.
pub(crate) const VERSION: u32 = 1;
