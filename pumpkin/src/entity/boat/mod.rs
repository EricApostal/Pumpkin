use pumpkin_entity::entity_type::EntityType;

#[derive(Debug, Clone, Copy)]
pub enum BoatType {
    Oak,
    OakChestBoat,
    Spruce,
    SpruceChestBoat,
    Birch,
    BirchChestBoat,
    Jungle,
    JungleChestBoat,
    Acacia,
    AcaciaChestBoat,
    DarkOak,
    DarkOakChestBoat,
    Mangrove,
    MangroveChestBoat,
    Bamboo,
    BambooChestRaft,
    Cherry,
    CherryChestBoat,
}

impl BoatType {
    pub fn to_entity_type(self) -> EntityType {
        match self {
            BoatType::Oak => EntityType::OakBoat,
            BoatType::OakChestBoat => EntityType::OakChestBoat,
            BoatType::Spruce => EntityType::SpruceBoat,
            BoatType::SpruceChestBoat => EntityType::SpruceChestBoat,
            BoatType::Birch => EntityType::BirchBoat,
            BoatType::BirchChestBoat => EntityType::BirchChestBoat,
            BoatType::Jungle => EntityType::JungleBoat,
            BoatType::JungleChestBoat => EntityType::JungleChestBoat,
            BoatType::Acacia => EntityType::AcaciaBoat,
            BoatType::AcaciaChestBoat => EntityType::AcaciaChestBoat,
            BoatType::DarkOak => EntityType::DarkOakBoat,
            BoatType::DarkOakChestBoat => EntityType::DarkOakChestBoat,
            BoatType::Mangrove => EntityType::MangroveBoat,
            BoatType::MangroveChestBoat => EntityType::MangroveChestBoat,
            BoatType::Bamboo => EntityType::BambooRaft,
            BoatType::BambooChestRaft => EntityType::BambooChestRaft,
            BoatType::Cherry => EntityType::CherryBoat,
            BoatType::CherryChestBoat => EntityType::CherryChestBoat,
        }
    }

    pub fn from_item_id(item_id: i32) -> Option<Self> {
        match item_id {
            803 => Some(BoatType::Oak),
            804 => Some(BoatType::OakChestBoat),
            805 => Some(BoatType::Spruce),
            806 => Some(BoatType::SpruceChestBoat),
            807 => Some(BoatType::Birch),
            808 => Some(BoatType::BirchChestBoat),
            809 => Some(BoatType::Jungle),
            810 => Some(BoatType::JungleChestBoat),
            811 => Some(BoatType::Acacia),
            812 => Some(BoatType::AcaciaChestBoat),
            813 => Some(BoatType::Cherry),
            814 => Some(BoatType::CherryChestBoat),
            815 => Some(BoatType::DarkOak),
            816 => Some(BoatType::DarkOakChestBoat),
            819 => Some(BoatType::Mangrove),
            820 => Some(BoatType::MangroveChestBoat),
            821 => Some(BoatType::Bamboo),
            822 => Some(BoatType::BambooChestRaft),

            _ => None,
        }
    }
}
