use eframe::egui;

const PREFERRED_FAMILIES: &[&str] = &[
    "Noto Sans CJK SC",
    "Noto Sans SC",
    "Source Han Sans SC",
    "Microsoft YaHei UI",
    "Microsoft YaHei",
    "PingFang SC",
    "WenQuanYi Micro Hei",
    "Droid Sans Fallback",
];

fn supports_chinese(database: &fontdb::Database, id: fontdb::ID) -> bool {
    database
        .with_face_data(id, |data, index| {
            ttf_parser::Face::parse(data, index).is_ok_and(|face| face.glyph_index('中').is_some())
        })
        .unwrap_or(false)
}

fn preferred_face(database: &fontdb::Database) -> Option<fontdb::ID> {
    PREFERRED_FAMILIES
        .iter()
        .find_map(|family| {
            let families = [fontdb::Family::Name(family)];
            database
                .query(&fontdb::Query {
                    families: &families,
                    ..Default::default()
                })
                .filter(|id| supports_chinese(database, *id))
        })
        .or_else(|| {
            database
                .faces()
                .map(|face| face.id)
                .find(|id| supports_chinese(database, *id))
        })
}

pub fn install(context: &egui::Context) {
    let mut database = fontdb::Database::new();
    database.load_system_fonts();

    let Some((data, index)) = preferred_face(&database)
        .and_then(|id| database.with_face_data(id, |data, index| (data.to_vec(), index)))
    else {
        return;
    };

    let mut font = egui::FontData::from_owned(data);
    font.index = index;

    let name = "system-cjk".to_owned();
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(name.clone(), font.into());
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.get_mut(&family).unwrap().push(name.clone());
    }
    context.set_fonts(fonts);
}
