use std::path::Path;

use lofty::config::WriteOptions;
use lofty::file::TaggedFile;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::*;
use lofty::read_from_path;
use lofty::tag::{ItemKey, Tag};

fn open_tagged(path: &Path) -> Result<TaggedFile, String> {
    read_from_path(path).map_err(|e| format!("Datei nicht lesbar: {}", e))
}

fn ensure_primary_tag(tagged: &mut TaggedFile) -> &mut Tag {
    let has_tag = tagged.primary_tag_mut().is_some() || tagged.first_tag_mut().is_some();
    if !has_tag {
        let tag_type = tagged.primary_tag_type();
        tagged.insert_tag(Tag::new(tag_type));
    }
    if tagged.primary_tag_mut().is_some() {
        tagged.primary_tag_mut().expect("checked above")
    } else {
        tagged.first_tag_mut().expect("tag just ensured")
    }
}

fn save(tagged: &TaggedFile, path: &Path) -> Result<(), String> {
    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| format!("Speichern fehlgeschlagen: {}", e))
}

/// Replace any existing cover art with the provided JPEG bytes.
pub fn embed_artwork(path: &Path, jpeg: &[u8]) -> Result<(), String> {
    let mut tagged = open_tagged(path)?;
    {
        let tag = ensure_primary_tag(&mut tagged);
        tag.remove_picture_type(PictureType::CoverFront);
        tag.remove_picture_type(PictureType::Other);
        tag.push_picture(Picture::new_unchecked(
            PictureType::CoverFront,
            Some(MimeType::Jpeg),
            None,
            jpeg.to_vec(),
        ));
    }
    save(&tagged, path)
}

/// Write lyrics into the file's lyrics tag (USLT/©lyr/LYRICS per format).
/// Timestamped LRC text is preferred so players can show synced lyrics.
pub fn embed_lyrics(path: &Path, text: &str) -> Result<(), String> {
    let mut tagged = open_tagged(path)?;
    {
        let tag = ensure_primary_tag(&mut tagged);
        tag.insert_text(ItemKey::Lyrics, text.to_string());
    }
    save(&tagged, path)
}
