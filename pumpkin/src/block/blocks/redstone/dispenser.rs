use rand::{Rng, RngExt, rng};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::block::blocks::carved_pumpkin::find_golem_pattern;
use crate::block::blocks::redstone::block_receives_redstone_power;
use crate::block::blocks::tnt::TNTBlock;
use crate::block::blocks::wither_skull::find_wither_pattern;
use crate::block::registry::BlockActionResult;
use crate::block::{
    BlockBehaviour, BlockFuture, GetComparatorOutputArgs, NormalUseArgs, OnNeighborUpdateArgs,
    OnPlaceArgs, OnScheduledTickArgs, PlacedArgs,
};
use crate::entity::decoration::armor_stand::ArmorStandEntity;
use crate::entity::item::ItemEntity;
use crate::entity::passive::sheep::SheepEntity;
use crate::entity::projectile::ThrownItemEntity;
use crate::entity::projectile::arrow::{ArrowEntity, ArrowPickup};
use crate::entity::projectile::egg::EggEntity;
use crate::entity::projectile::firework_rocket::FireworkRocketEntity;
use crate::entity::projectile::lingering_potion::LingeringPotionEntity;
use crate::entity::projectile::small_fireball::SmallFireballEntity;
use crate::entity::projectile::snowball::SnowballEntity;
use crate::entity::projectile::splash_potion::SplashPotionEntity;
use crate::entity::projectile::wind_charge::{WIND_CHARGE_GRAVITY, WindChargeEntity};
use crate::entity::tnt::TNTEntity;
use crate::entity::r#type::from_type;
use crate::entity::vehicle::boat::BoatEntity;
use crate::entity::vehicle::minecart::MinecartEntity;
use crate::entity::{Entity, EntityBase};
use crate::item::ItemMetadata;
use crate::item::items::boat::BoatItem;
use crate::item::items::bucket::{
    FilledBucketItem, play_bucket_evaporation, should_evaporate_in_nether, try_pickup_bucket_item,
    try_place_filled_bucket,
};
use crate::item::items::honeycomb::get_waxed_state;
use crate::item::items::ignite::ignition::Ignition;
use crate::item::items::minecart::MinecartItem;
use crate::item::items::spawn_egg::apply_entity_variant;
use crate::world::World;

use crate::block::entities::dispenser::DispenserBlockEntity;
use pumpkin_data::block_properties::{
    BeeNestLikeProperties, BlockProperties, EndRodLikeProperties, Facing,
    PoweredRailLikeProperties, RailLikeProperties, RespawnAnchorLikeProperties,
    SkeletonSkullLikeProperties,
};
use pumpkin_data::data_component::DataComponent;
use pumpkin_data::data_component_impl::{EquippableImpl, IDSet, PotionContentsImpl};
use pumpkin_data::entity::{EntityType, entity_from_egg};
use pumpkin_data::fluid::Fluid;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::particle::Particle;
use pumpkin_data::potion::Potion;
use pumpkin_data::sound::{Sound, SoundCategory};
use pumpkin_data::tag::Taggable;
use pumpkin_data::world::WorldEvent;
use pumpkin_data::{Block, BlockStateId, FacingExt, tag, translation};
use pumpkin_inventory::generic_container_screen_handler::create_generic_3x3;
use pumpkin_inventory::player::player_inventory::PlayerInventory;
use pumpkin_inventory::screen_handler::{
    BoxFuture, InventoryPlayer, ScreenHandlerFactory, SharedScreenHandler,
};
use pumpkin_macros::pumpkin_block;
use pumpkin_util::math::boundingbox::{BoundingBox, EntityDimensions};
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::math::wrap_degrees;
use pumpkin_util::text::TextComponent;
use pumpkin_world::inventory::Inventory;
use pumpkin_world::tick::TickPriority;
use pumpkin_world::world::BlockFlags;

struct DispenserScreenFactory(Arc<dyn Inventory>);

impl ScreenHandlerFactory for DispenserScreenFactory {
    fn create_screen_handler<'a>(
        &'a self,
        sync_id: u8,
        player_inventory: &'a Arc<PlayerInventory>,
        _player: &'a dyn InventoryPlayer,
    ) -> BoxFuture<'a, Option<SharedScreenHandler>> {
        Box::pin(async move {
            let handler = create_generic_3x3(sync_id, player_inventory, self.0.clone()).await;
            let screen_handler_arc = Arc::new(Mutex::new(handler));

            Some(screen_handler_arc as SharedScreenHandler)
        })
    }

    fn get_display_name(&self) -> TextComponent {
        TextComponent::translate_cross(
            translation::java::CONTAINER_DISPENSER,
            translation::bedrock::CONTAINER_DISPENSER,
            &[],
        )
    }
}

#[pumpkin_block("minecraft:dispenser")]
pub struct DispenserBlock;

type DispenserLikeProperties = pumpkin_data::block_properties::DispenserLikeProperties;

struct DispenseContext<'a> {
    world: &'a Arc<World>,
    position: &'a BlockPos,
    facing: Facing,
}

impl<'a> DispenseContext<'a> {
    const fn new(args: &OnScheduledTickArgs<'a>, facing: Facing) -> Self {
        Self {
            world: args.world,
            position: args.position,
            facing,
        }
    }
}

enum DispenseOutcome {
    Success(WorldEvent),
    Failure,
    Drop,
    DropFailure,
}

fn triangle<R: Rng>(rng: &mut R, min: f64, max: f64) -> f64 {
    (rng.random::<f64>() - rng.random::<f64>()).mul_add(max, min)
}

const fn to_normal(facing: Facing) -> Vector3<f64> {
    match facing {
        Facing::North => Vector3::new(0., 0., -1.),
        Facing::East => Vector3::new(1., 0., 0.),
        Facing::South => Vector3::new(0., 0., 1.),
        Facing::West => Vector3::new(-1., 0., 0.),
        Facing::Up => Vector3::new(0., 1., 0.),
        Facing::Down => Vector3::new(0., -1., 0.),
    }
}

const fn to_data3d(facing: Facing) -> i32 {
    match facing {
        Facing::North => 2,
        Facing::East => 5,
        Facing::South => 3,
        Facing::West => 4,
        Facing::Up => 1,
        Facing::Down => 0,
    }
}

const fn to_rotation16(facing: Facing) -> u8 {
    match facing {
        Facing::South | Facing::Up | Facing::Down => 0,
        Facing::West => 4,
        Facing::North => 8,
        Facing::East => 12,
    }
}

const fn is_water(fluid: &Fluid) -> bool {
    fluid.id == Fluid::WATER.id || fluid.id == Fluid::FLOWING_WATER.id
}

fn is_allowed_entity(allowed: Option<&IDSet<EntityType>>, entity_type: &EntityType) -> bool {
    match allowed {
        None => true,
        Some(IDSet::Tag(tag)) => entity_type.is_tagged_with(tag).unwrap_or(false),
        Some(IDSet::IDs(types)) => types.contains(&entity_type),
    }
}

const fn wool_of_color(color: u8) -> &'static Item {
    match color {
        1 => &Item::ORANGE_WOOL,
        2 => &Item::MAGENTA_WOOL,
        3 => &Item::LIGHT_BLUE_WOOL,
        4 => &Item::YELLOW_WOOL,
        5 => &Item::LIME_WOOL,
        6 => &Item::PINK_WOOL,
        7 => &Item::GRAY_WOOL,
        8 => &Item::LIGHT_GRAY_WOOL,
        9 => &Item::CYAN_WOOL,
        10 => &Item::PURPLE_WOOL,
        11 => &Item::BLUE_WOOL,
        12 => &Item::BROWN_WOOL,
        13 => &Item::GREEN_WOOL,
        14 => &Item::RED_WOOL,
        15 => &Item::BLACK_WOOL,
        _ => &Item::WHITE_WOOL,
    }
}

fn water_bottle() -> ItemStack {
    ItemStack::new_with_component(
        1,
        &Item::POTION,
        vec![(
            DataComponent::PotionContents,
            Some(Box::new(PotionContentsImpl {
                potion_id: Some(i32::from(Potion::WATER.id)),
                custom_color: None,
                custom_effects: Vec::new(),
                custom_name: None,
            }) as Box<_>),
        )],
    )
}

fn is_water_bottle(stack: &ItemStack) -> bool {
    stack.item.id == Item::POTION.id
        && stack
            .get_data_component::<PotionContentsImpl>()
            .and_then(|contents| contents.potion_id)
            == Some(i32::from(Potion::WATER.id))
}

impl BlockBehaviour for DispenserBlock {
    fn normal_use<'a>(&'a self, args: NormalUseArgs<'a>) -> BlockFuture<'a, BlockActionResult> {
        Box::pin(async move {
            if let Some(block_entity) = args.world.get_block_entity(args.position)
                && let Some(inventory) = block_entity.get_inventory()
            {
                args.player
                    .open_handled_screen(&DispenserScreenFactory(inventory), Some(*args.position))
                    .await;
            }
            BlockActionResult::Success
        })
    }

    fn on_place<'a>(&'a self, args: OnPlaceArgs<'a>) -> BlockFuture<'a, BlockStateId> {
        Box::pin(async move {
            let mut props = DispenserLikeProperties::default(args.block);
            props.facing = args.player.get_entity().get_facing().opposite();
            props.to_state_id(args.block)
        })
    }

    fn placed<'a>(&'a self, args: PlacedArgs<'a>) -> BlockFuture<'a, ()> {
        Box::pin(async move {
            let dispenser_block_entity = DispenserBlockEntity::new(*args.position);
            args.world
                .add_block_entity(Arc::new(dispenser_block_entity));
        })
    }

    fn on_neighbor_update<'a>(&'a self, args: OnNeighborUpdateArgs<'a>) -> BlockFuture<'a, ()> {
        Box::pin(async move {
            let powered = block_receives_redstone_power(args.world, args.position).await
                || block_receives_redstone_power(args.world, &args.position.up()).await;

            let mut props = DispenserLikeProperties::from_state_id(
                args.world.get_block_state(args.position).id,
                args.block,
            );

            if powered && !props.triggered {
                args.world
                    .schedule_block_tick(args.block, *args.position, 4, TickPriority::Normal);
                props.triggered = true;
                args.world
                    .set_block_state(
                        args.position,
                        props.to_state_id(args.block),
                        BlockFlags::NOTIFY_LISTENERS,
                    )
                    .await;
            } else if !powered && props.triggered {
                props.triggered = false;
                args.world
                    .set_block_state(
                        args.position,
                        props.to_state_id(args.block),
                        BlockFlags::NOTIFY_LISTENERS,
                    )
                    .await;
            }
        })
    }

    fn on_scheduled_tick<'a>(&'a self, args: OnScheduledTickArgs<'a>) -> BlockFuture<'a, ()> {
        Box::pin(async move {
            if let Some(block_entity) = args.world.get_block_entity(args.position) {
                let Some(dispenser) = block_entity.as_any().downcast_ref::<DispenserBlockEntity>()
                else {
                    return;
                };

                if let Some((slot, mut item)) = dispenser.get_random_slot().await {
                    let props = DispenserLikeProperties::from_state_id(
                        args.world.get_block_state(args.position).id,
                        args.block,
                    );
                    let ctx = DispenseContext::new(&args, props.facing);

                    Self::dispense(&ctx, dispenser, slot, &mut item).await;
                } else {
                    args.world
                        .sync_world_event(WorldEvent::SoundDispenserFail, *args.position, 0);
                }
            }
        })
    }

    fn get_comparator_output<'a>(
        &'a self,
        args: GetComparatorOutputArgs<'a>,
    ) -> BlockFuture<'a, Option<u8>> {
        Box::pin(async move {
            if let Some(block_entity) = args.world.get_block_entity(args.position)
                && let Some(inventory) = block_entity.get_inventory()
            {
                Some(crate::block::calculate_comparator_output(inventory.as_ref()).await)
            } else {
                None
            }
        })
    }
}

impl DispenserBlock {
    const PROJECTILE_POWER: f64 = 1.1;
    const PROJECTILE_UNCERTAINTY: f64 = 6.0;
    const POTION_POWER: f64 = Self::PROJECTILE_POWER * 1.25;
    const POTION_UNCERTAINTY: f64 = Self::PROJECTILE_UNCERTAINTY * 0.5;
    const FIREWORK_POWER: f64 = 0.5;
    const FIREWORK_UNCERTAINTY: f64 = 1.0;
    const FIRE_CHARGE_POWER: f64 = 1.0;
    const FIRE_CHARGE_UNCERTAINTY: f64 = 6.6666665;

    async fn dispense(
        ctx: &DispenseContext<'_>,
        dispenser: &DispenserBlockEntity,
        slot: usize,
        item: &mut ItemStack,
    ) {
        let sound = match Self::run_behavior(ctx, dispenser, slot, item).await {
            DispenseOutcome::Success(event) => event,
            DispenseOutcome::Failure => WorldEvent::SoundDispenserFail,
            outcome @ (DispenseOutcome::Drop | DispenseOutcome::DropFailure) => {
                Self::drop_stack(ctx, item.split(1)).await;
                if matches!(outcome, DispenseOutcome::Drop) {
                    WorldEvent::SoundDispenserDispense
                } else {
                    WorldEvent::SoundDispenserFail
                }
            }
        };

        ctx.world.sync_world_event(sound, *ctx.position, 0);
        ctx.world.sync_world_event(
            WorldEvent::ParticlesShootSmoke,
            *ctx.position,
            to_data3d(ctx.facing),
        );
        dispenser.mark_dirty();
    }

    /// Still missing some specific dispenser behavior that you can find here:
    /// <https://minecraft.wiki/w/Dispenser#Usage>
    async fn run_behavior(
        ctx: &DispenseContext<'_>,
        dispenser: &DispenserBlockEntity,
        slot: usize,
        item: &mut ItemStack,
    ) -> DispenseOutcome {
        const DISPENSE: WorldEvent = WorldEvent::SoundDispenserDispense;

        let id = item.item.id;
        let projectiles = [
            Item::ARROW.id,
            Item::TIPPED_ARROW.id,
            Item::SPECTRAL_ARROW.id,
            Item::EGG.id,
            Item::BLUE_EGG.id,
            Item::BROWN_EGG.id,
            Item::SNOWBALL.id,
            Item::SPLASH_POTION.id,
            Item::LINGERING_POTION.id,
            Item::FIREWORK_ROCKET.id,
            Item::FIRE_CHARGE.id,
            Item::WIND_CHARGE.id,
        ];

        if projectiles.contains(&id) {
            return DispenseOutcome::Success(Self::dispense_projectile(ctx, item).await);
        }

        if BoatItem::ids().contains(&id) {
            return if Self::dispense_boat(ctx, item).await {
                DispenseOutcome::Success(DISPENSE)
            } else {
                DispenseOutcome::Drop
            };
        }

        if MinecartItem::ids().contains(&id) {
            return if Self::dispense_minecart(ctx, item).await {
                DispenseOutcome::Success(DISPENSE)
            } else {
                DispenseOutcome::Drop
            };
        }

        if id == Item::ARMOR_STAND.id {
            return if Self::dispense_armor_stand(ctx, item).await {
                DispenseOutcome::Success(DISPENSE)
            } else {
                DispenseOutcome::Drop
            };
        }

        if id == Item::TNT.id {
            Self::dispense_tnt(ctx, item).await;
            return DispenseOutcome::Success(DISPENSE);
        }

        if entity_from_egg(id).is_some() {
            Self::dispense_spawn_egg(ctx, item).await;
            return DispenseOutcome::Success(DISPENSE);
        }

        if let Some(outcome) = Self::run_world_behavior(ctx, dispenser, slot, item).await {
            return outcome;
        }

        if Self::dispense_equipment(ctx, item).await {
            return DispenseOutcome::Success(DISPENSE);
        }

        // TODO: Missing behaviors
        // Bottles o enchanting
        // Chests onto llamas, donkeys and mules
        // Brushes onto armadillos
        DispenseOutcome::Drop
    }

    async fn run_world_behavior(
        ctx: &DispenseContext<'_>,
        dispenser: &DispenserBlockEntity,
        slot: usize,
        item: &mut ItemStack,
    ) -> Option<DispenseOutcome> {
        const DISPENSE: WorldEvent = WorldEvent::SoundDispenserDispense;
        let id = item.item.id;

        if id == Item::BUCKET.id {
            return Some(
                if Self::dispense_empty_bucket(ctx, dispenser, slot, item).await {
                    DispenseOutcome::Success(DISPENSE)
                } else {
                    DispenseOutcome::Drop
                },
            );
        }

        if FilledBucketItem::ids().contains(&id) {
            return Some(if Self::dispense_filled_bucket(ctx, item).await {
                DispenseOutcome::Success(DISPENSE)
            } else {
                DispenseOutcome::Drop
            });
        }

        if id == Item::FLINT_AND_STEEL.id {
            return Some(if Self::dispense_flint_and_steel(ctx, item).await {
                DispenseOutcome::Success(DISPENSE)
            } else {
                DispenseOutcome::Failure
            });
        }

        if id == Item::SHEARS.id {
            return Some(if Self::dispense_shears(ctx, item).await {
                DispenseOutcome::Success(DISPENSE)
            } else {
                DispenseOutcome::Failure
            });
        }

        if id == Item::GLASS_BOTTLE.id {
            return Some(
                if Self::dispense_glass_bottle(ctx, dispenser, slot, item).await {
                    DispenseOutcome::Success(DISPENSE)
                } else {
                    DispenseOutcome::DropFailure
                },
            );
        }

        if is_water_bottle(item) {
            return Some(if Self::dispense_water_bottle(ctx, item).await {
                DispenseOutcome::Success(DISPENSE)
            } else {
                DispenseOutcome::Drop
            });
        }

        if id == Item::HONEYCOMB.id {
            return Some(if Self::dispense_honeycomb(ctx, item).await {
                DispenseOutcome::Success(DISPENSE)
            } else {
                DispenseOutcome::Drop
            });
        }

        if id == Item::GLOWSTONE.id {
            return Some(match Self::dispense_glowstone(ctx, item).await {
                Some(true) => DispenseOutcome::Success(DISPENSE),
                Some(false) => DispenseOutcome::Failure,
                None => DispenseOutcome::Drop,
            });
        }

        if id == Item::BONE_MEAL.id {
            // TODO: Bone meal
            return Some(DispenseOutcome::Failure);
        }

        if id == Item::WITHER_SKELETON_SKULL.id {
            return Some(Self::dispense_mob_head(ctx, item, &Block::WITHER_SKELETON_SKULL).await);
        }

        if id == Item::CARVED_PUMPKIN.id {
            return Some(Self::dispense_mob_head(ctx, item, &Block::CARVED_PUMPKIN).await);
        }

        if Block::from_item_id(id)
            .is_some_and(|block| block.has_tag(&tag::Block::MINECRAFT_SHULKER_BOXES))
        {
            return Some(if Self::dispense_shulker_box(ctx, item).await {
                DispenseOutcome::Success(DISPENSE)
            } else {
                DispenseOutcome::Failure
            });
        }

        None
    }

    fn shoot(entity: &Entity, direction: Vector3<f64>, power: f64, uncertainty: f64) {
        let spread = 0.017_227_5 * uncertainty;
        let mut rng = rng();
        let velocity = direction
            .normalize()
            .add_raw(
                triangle(&mut rng, 0.0, spread),
                triangle(&mut rng, 0.0, spread),
                triangle(&mut rng, 0.0, spread),
            )
            .multiply(power, power, power);

        entity.velocity.store(velocity);
        let length = velocity.horizontal_length();
        entity.set_rotation(
            velocity.x.atan2(velocity.z) as f32 * 57.295776,
            velocity.y.atan2(length) as f32 * 57.295776,
        );
    }

    fn dispense_position(ctx: &DispenseContext<'_>) -> Vector3<f64> {
        ctx.position
            .to_centered_f64()
            .add(&(to_normal(ctx.facing) * 0.7))
    }

    async fn dispense_projectile(ctx: &DispenseContext<'_>, item: &mut ItemStack) -> WorldEvent {
        let stack = item.split(1);
        let position = Self::dispense_position(ctx);
        let world = ctx.world;
        let id = stack.item.id;

        let (projectile, power, uncertainty, sound): (Arc<dyn EntityBase>, f64, f64, WorldEvent) =
            if id == Item::SNOWBALL.id {
                let entity = Entity::new(world.clone(), position, &EntityType::SNOWBALL);
                (
                    Arc::new(SnowballEntity::new(entity)),
                    Self::PROJECTILE_POWER,
                    Self::PROJECTILE_UNCERTAINTY,
                    WorldEvent::SoundDispenserProjectileLaunch,
                )
            } else if id == Item::EGG.id || id == Item::BLUE_EGG.id || id == Item::BROWN_EGG.id {
                let entity = Entity::new(world.clone(), position, &EntityType::EGG);
                let egg = EggEntity::new(entity);
                egg.set_item_stack(stack.clone()).await;
                (
                    Arc::new(egg),
                    Self::PROJECTILE_POWER,
                    Self::PROJECTILE_UNCERTAINTY,
                    WorldEvent::SoundDispenserProjectileLaunch,
                )
            } else if id == Item::SPLASH_POTION.id {
                let entity = Entity::new(world.clone(), position, &EntityType::SPLASH_POTION);
                let potion = SplashPotionEntity::new(entity);
                potion.set_item_stack(stack.clone()).await;
                (
                    Arc::new(potion),
                    Self::POTION_POWER,
                    Self::POTION_UNCERTAINTY,
                    WorldEvent::SoundDispenserProjectileLaunch,
                )
            } else if id == Item::LINGERING_POTION.id {
                let entity = Entity::new(world.clone(), position, &EntityType::LINGERING_POTION);
                let potion = LingeringPotionEntity::new(entity);
                potion.set_item_stack(stack.clone()).await;
                (
                    Arc::new(potion),
                    Self::POTION_POWER,
                    Self::POTION_UNCERTAINTY,
                    WorldEvent::SoundDispenserProjectileLaunch,
                )
            } else if id == Item::FIREWORK_ROCKET.id {
                let entity = Entity::new(world.clone(), position, &EntityType::FIREWORK_ROCKET);
                (
                    Arc::new(FireworkRocketEntity::new(entity)),
                    Self::FIREWORK_POWER,
                    Self::FIREWORK_UNCERTAINTY,
                    WorldEvent::SoundFireworkShoot,
                )
            } else if id == Item::FIRE_CHARGE.id {
                let entity = Entity::new(world.clone(), position, &EntityType::SMALL_FIREBALL);
                (
                    Arc::new(SmallFireballEntity::new(entity)),
                    Self::FIRE_CHARGE_POWER,
                    Self::FIRE_CHARGE_UNCERTAINTY,
                    WorldEvent::SoundBlazeFireball,
                )
            } else if id == Item::WIND_CHARGE.id {
                let entity = Entity::new(world.clone(), position, &EntityType::WIND_CHARGE);
                let thrown = ThrownItemEntity {
                    entity,
                    owner_id: None,
                    collides_with_projectiles: false,
                    has_hit: AtomicBool::new(false),
                    gravity: WIND_CHARGE_GRAVITY,
                };
                (
                    Arc::new(WindChargeEntity::new_normal(thrown)),
                    Self::POTION_POWER,
                    Self::POTION_UNCERTAINTY,
                    WorldEvent::SoundWindChargeShoot,
                )
            } else {
                // TODO: Tipped arrows
                let entity_type = if id == Item::SPECTRAL_ARROW.id {
                    &EntityType::SPECTRAL_ARROW
                } else {
                    &EntityType::ARROW
                };
                let entity = Entity::new(world.clone(), position, entity_type);
                let mut arrow = ArrowEntity::new(entity, None);
                arrow.pickup = ArrowPickup::Allowed;
                (
                    Arc::new(arrow),
                    Self::PROJECTILE_POWER,
                    Self::PROJECTILE_UNCERTAINTY,
                    WorldEvent::SoundDispenserProjectileLaunch,
                )
            };

        let mut direction = to_normal(ctx.facing);
        direction.y += 0.1;

        Self::shoot(projectile.get_entity(), direction, power, uncertainty);
        world.spawn_entity(projectile).await;

        sound
    }

    fn target_position(ctx: &DispenseContext<'_>) -> BlockPos {
        let facing = to_normal(ctx.facing);
        ctx.position.offset(Vector3::new(
            facing.x as i32,
            facing.y as i32,
            facing.z as i32,
        ))
    }

    fn has_room_for(
        ctx: &DispenseContext<'_>,
        spawn_pos: Vector3<f64>,
        size: &EntityDimensions,
    ) -> bool {
        let bounding_box = BoundingBox::new_from_pos(spawn_pos.x, spawn_pos.y, spawn_pos.z, size);
        ctx.world.is_space_empty(bounding_box)
            && ctx.world.get_entities_at_box(&bounding_box).is_empty()
    }

    fn placement_yaw(ctx: &DispenseContext<'_>) -> f32 {
        let facing = to_normal(ctx.facing);
        facing.x.atan2(facing.z) as f32 * 57.295776
    }

    async fn dispense_boat(ctx: &DispenseContext<'_>, item: &mut ItemStack) -> bool {
        let target = Self::target_position(ctx);

        let spawn_pos = if is_water(ctx.world.get_fluid(&target)) {
            target.to_f64()
        } else if ctx.world.get_block_state(&target).is_air()
            && is_water(ctx.world.get_fluid(&target.down()))
        {
            target.down().to_f64()
        } else {
            return false;
        };

        let entity_type = BoatItem::item_to_entity(item.item);
        let dimensions = EntityDimensions::new(
            entity_type.dimension[0],
            entity_type.dimension[1],
            entity_type.eye_height,
        );
        if !Self::has_room_for(ctx, spawn_pos, &dimensions) {
            return false;
        }

        let _ = item.split(1);
        let entity = Entity::new(ctx.world.clone(), spawn_pos, entity_type);
        entity.set_rotation(Self::placement_yaw(ctx), 0.0);
        ctx.world
            .spawn_entity(Arc::new(BoatEntity::new(entity)))
            .await;

        true
    }

    async fn dispense_minecart(ctx: &DispenseContext<'_>, item: &mut ItemStack) -> bool {
        fn rail_is_ascending(world: &Arc<World>, pos: &BlockPos) -> Option<bool> {
            let (block, state_id) = world.get_block_and_state_id(pos);
            if !block.has_tag(&tag::Block::MINECRAFT_RAILS) {
                return None;
            }
            Some(if PoweredRailLikeProperties::handles_block_id(block.id) {
                PoweredRailLikeProperties::from_state_id(state_id, block)
                    .shape
                    .is_ascending()
            } else {
                RailLikeProperties::from_state_id(state_id, block)
                    .shape
                    .is_ascending()
            })
        }

        let target = Self::target_position(ctx);
        let height = if let Some(ascending) = rail_is_ascending(ctx.world, &target) {
            if ascending { 0.6 } else { 0.1 }
        } else if ctx.world.get_block_state(&target).is_air()
            && let Some(ascending) = rail_is_ascending(ctx.world, &target.down())
        {
            if ascending && ctx.facing != Facing::Down {
                -0.4
            } else {
                -0.9
            }
        } else {
            return false;
        };

        let entity_type = MinecartItem::item_to_entity(item.item);
        let _ = item.split(1);

        let normal = to_normal(ctx.facing);
        let center = ctx.position.to_centered_f64();
        let spawn_pos = Vector3::new(
            normal.x.mul_add(1.125, center.x),
            f64::from(ctx.position.0.y) + normal.y + height,
            normal.z.mul_add(1.125, center.z),
        );

        let entity = Entity::new(ctx.world.clone(), spawn_pos, entity_type);
        ctx.world
            .spawn_entity(Arc::new(MinecartEntity::new(entity)))
            .await;

        true
    }

    async fn dispense_armor_stand(ctx: &DispenseContext<'_>, item: &mut ItemStack) -> bool {
        let target = Self::target_position(ctx);
        let spawn_pos = target.to_f64();
        let dimensions = EntityDimensions::new(
            EntityType::ARMOR_STAND.dimension[0],
            EntityType::ARMOR_STAND.dimension[1],
            EntityType::ARMOR_STAND.eye_height,
        );
        if !Self::has_room_for(ctx, spawn_pos, &dimensions) {
            return false;
        }

        let _ = item.split(1);
        let entity = Entity::new(ctx.world.clone(), spawn_pos, &EntityType::ARMOR_STAND);
        entity.set_rotation(Self::placement_yaw(ctx), 0.0);

        ctx.world.play_sound(
            Sound::EntityArmorStandPlace,
            SoundCategory::Blocks,
            &spawn_pos,
        );
        ctx.world
            .spawn_entity(Arc::new(ArmorStandEntity::new(entity)))
            .await;

        true
    }

    async fn dispense_tnt(ctx: &DispenseContext<'_>, item: &mut ItemStack) {
        const TNT_POWER: f32 = 4.0;
        const TNT_FUSE: u32 = 80;

        let _ = item.split(1);
        let spawn_pos = Self::target_position(ctx).to_f64();

        let entity = Entity::new(ctx.world.clone(), spawn_pos, &EntityType::TNT);
        let tnt = Arc::new(TNTEntity::new(entity, TNT_POWER, TNT_FUSE));
        ctx.world.spawn_entity(tnt).await;
        ctx.world
            .play_sound(Sound::EntityTntPrimed, SoundCategory::Blocks, &spawn_pos);
    }

    async fn dispense_spawn_egg(ctx: &DispenseContext<'_>, item: &mut ItemStack) {
        let Some(entity_type) = entity_from_egg(item.item.id) else {
            return;
        };

        let _ = item.split(1);
        let spawn_pos = Self::target_position(ctx).to_f64();

        let mob = from_type(entity_type, spawn_pos, ctx.world, Uuid::new_v4());
        let yaw = wrap_degrees(rng().random::<f32>() * 360.0) % 360.0;
        mob.get_entity().set_rotation(yaw, 0.0);
        apply_entity_variant(item, mob.as_ref());

        ctx.world.spawn_entity(mob).await;
    }

    async fn dispense_empty_bucket(
        ctx: &DispenseContext<'_>,
        dispenser: &DispenserBlockEntity,
        slot: usize,
        item: &mut ItemStack,
    ) -> bool {
        let target = Self::target_position(ctx);
        let Some(filled) =
            try_pickup_bucket_item(ctx.world, target, ctx.facing.to_block_direction()).await
        else {
            return false;
        };

        Self::consume_with_remainder(ctx, dispenser, slot, item, ItemStack::new(1, filled)).await;
        true
    }

    async fn dispense_filled_bucket(ctx: &DispenseContext<'_>, item: &mut ItemStack) -> bool {
        let target = Self::target_position(ctx);

        if should_evaporate_in_nether(item.item, ctx.world) {
            play_bucket_evaporation(ctx.world, &target.to_centered_f64());
        } else if !try_place_filled_bucket(
            ctx.world,
            item.item,
            *ctx.position,
            ctx.facing.to_block_direction(),
        )
        .await
        {
            return false;
        }

        // TODO: Mob buckets e.g. axolotl buckets
        *item = ItemStack::new(1, &Item::BUCKET);
        true
    }

    async fn dispense_flint_and_steel(ctx: &DispenseContext<'_>, item: &mut ItemStack) -> bool {
        let target = Self::target_position(ctx);

        let ignited = if ctx.world.get_block(&target) == &Block::TNT {
            TNTBlock::prime(ctx.world, &target).await;
            true
        } else {
            Ignition::ignite_position(
                |world: Arc<World>, pos: BlockPos, new_state_id: BlockStateId| async move {
                    world
                        .set_block_state(&pos, new_state_id, BlockFlags::NOTIFY_ALL)
                        .await;
                },
                ctx.world,
                target,
            )
            .await
        };

        if ignited {
            let _ = item.damage_item(1);
        }
        ignited
    }

    async fn dispense_shears(ctx: &DispenseContext<'_>, item: &mut ItemStack) -> bool {
        let sheared = Self::shear_beehive(ctx).await || Self::shear_entity_in_front(ctx).await;

        if sheared {
            let _ = item.damage_item(1);
        }
        sheared
    }

    async fn shear_beehive(ctx: &DispenseContext<'_>) -> bool {
        const FULL_HONEY_LEVEL: u8 = 5;
        const HARVESTED_HONEYCOMBS: u8 = 3;

        let target = Self::target_position(ctx);
        let (block, state_id) = ctx.world.get_block_and_state_id(&target);
        if !block.has_tag(&tag::Block::MINECRAFT_BEEHIVES) {
            return false;
        }

        let mut props = BeeNestLikeProperties::from_state_id(state_id, block);
        if props.honey_level < FULL_HONEY_LEVEL {
            return false;
        }

        ctx.world
            .play_block_sound(Sound::BlockBeehiveShear, SoundCategory::Blocks, target);
        Self::drop_at(
            ctx.world,
            target.to_centered_f64(),
            ItemStack::new(HARVESTED_HONEYCOMBS, &Item::HONEYCOMB),
        )
        .await;

        props.honey_level = 0;
        ctx.world
            .set_block_state(&target, props.to_state_id(block), BlockFlags::NOTIFY_ALL)
            .await;

        true
    }

    async fn shear_entity_in_front(ctx: &DispenseContext<'_>) -> bool {
        let target_box = BoundingBox::from_block(&Self::target_position(ctx));

        for entity in ctx.world.get_entities_at_box(&target_box) {
            let Some(sheep) = entity.cast_any().downcast_ref::<SheepEntity>() else {
                continue;
            };
            if sheep.is_sheared() || !entity.get_entity().is_alive() {
                continue;
            }

            let position = entity.get_entity().pos.load();
            sheep.set_sheared(true);
            ctx.world
                .play_sound(Sound::EntitySheepShear, SoundCategory::Blocks, &position);

            let count = rng().random_range(1..=3);
            Self::drop_at(
                ctx.world,
                position,
                ItemStack::new(count, wool_of_color(sheep.get_color())),
            )
            .await;

            return true;
        }

        false
    }

    async fn dispense_glass_bottle(
        ctx: &DispenseContext<'_>,
        dispenser: &DispenserBlockEntity,
        slot: usize,
        item: &mut ItemStack,
    ) -> bool {
        const FULL_HONEY_LEVEL: u8 = 5;

        let target = Self::target_position(ctx);
        let (block, state_id) = ctx.world.get_block_and_state_id(&target);

        if block.has_tag(&tag::Block::MINECRAFT_BEEHIVES) {
            let mut props = BeeNestLikeProperties::from_state_id(state_id, block);
            if props.honey_level < FULL_HONEY_LEVEL {
                return false;
            }

            props.honey_level = 0;
            ctx.world
                .set_block_state(&target, props.to_state_id(block), BlockFlags::NOTIFY_ALL)
                .await;
            Self::consume_with_remainder(
                ctx,
                dispenser,
                slot,
                item,
                ItemStack::new(1, &Item::HONEY_BOTTLE),
            )
            .await;
            return true;
        }

        if is_water(ctx.world.get_fluid(&target)) {
            Self::consume_with_remainder(ctx, dispenser, slot, item, water_bottle()).await;
            return true;
        }

        false
    }

    async fn dispense_water_bottle(ctx: &DispenseContext<'_>, item: &mut ItemStack) -> bool {
        let target = Self::target_position(ctx);
        if !ctx
            .world
            .get_block(&target)
            .has_tag(&tag::Block::MINECRAFT_CONVERTABLE_TO_MUD)
        {
            return false;
        }

        ctx.world.spawn_particle(
            target.to_centered_f64(),
            Vector3::new(0.5, 0.5, 0.5),
            1.0,
            5,
            Particle::Splash,
        );
        ctx.world
            .play_block_sound(Sound::ItemBottleEmpty, SoundCategory::Blocks, target);
        ctx.world
            .set_block_state(&target, Block::MUD.default_state.id, BlockFlags::NOTIFY_ALL)
            .await;

        *item = ItemStack::new(1, &Item::GLASS_BOTTLE);
        true
    }

    async fn dispense_honeycomb(ctx: &DispenseContext<'_>, item: &mut ItemStack) -> bool {
        let target = Self::target_position(ctx);
        let (block, state_id) = ctx.world.get_block_and_state_id(&target);
        let Some(waxed_state_id) = get_waxed_state(block, state_id) else {
            return false;
        };

        let _ = item.split(1);
        ctx.world
            .set_block_state(&target, waxed_state_id, BlockFlags::NOTIFY_ALL)
            .await;
        ctx.world
            .sync_world_event(WorldEvent::ParticlesAndSoundWaxOn, target, 0);

        true
    }

    async fn dispense_glowstone(ctx: &DispenseContext<'_>, item: &mut ItemStack) -> Option<bool> {
        const MAX_CHARGES: u8 = 4;

        let target = Self::target_position(ctx);
        let (block, state_id) = ctx.world.get_block_and_state_id(&target);
        if block != &Block::RESPAWN_ANCHOR {
            return None;
        }

        let mut props = RespawnAnchorLikeProperties::from_state_id(state_id, block);
        if props.charges >= MAX_CHARGES {
            return Some(false);
        }

        props.charges += 1;
        let _ = item.split(1);
        ctx.world
            .set_block_state(&target, props.to_state_id(block), BlockFlags::NOTIFY_ALL)
            .await;
        ctx.world.play_block_sound(
            Sound::BlockRespawnAnchorCharge,
            SoundCategory::Blocks,
            target,
        );

        Some(true)
    }

    async fn dispense_mob_head(
        ctx: &DispenseContext<'_>,
        item: &mut ItemStack,
        block: &'static Block,
    ) -> DispenseOutcome {
        let target = Self::target_position(ctx);

        let summons_mob = ctx.world.get_block_state(&target).is_air()
            && if block == &Block::WITHER_SKELETON_SKULL {
                find_wither_pattern(ctx.world, &target).is_some()
            } else {
                find_golem_pattern(ctx.world, &target).is_some()
            };

        if summons_mob {
            let state_id = if block == &Block::WITHER_SKELETON_SKULL {
                let mut props = SkeletonSkullLikeProperties::default(block);
                props.rotation = to_rotation16(ctx.facing);
                props.to_state_id(block)
            } else {
                block.default_state.id
            };

            let _ = item.split(1);
            ctx.world
                .set_block_state(&target, state_id, BlockFlags::NOTIFY_ALL)
                .await;
            return DispenseOutcome::Success(WorldEvent::SoundDispenserDispense);
        }

        if Self::dispense_equipment(ctx, item).await {
            DispenseOutcome::Success(WorldEvent::SoundDispenserDispense)
        } else {
            DispenseOutcome::Failure
        }
    }

    async fn dispense_shulker_box(ctx: &DispenseContext<'_>, item: &mut ItemStack) -> bool {
        let Some(block) = Block::from_item_id(item.item.id) else {
            return false;
        };

        let target = Self::target_position(ctx);
        if !ctx.world.get_block_state(&target).replaceable() {
            return false;
        }

        let mut props = EndRodLikeProperties::default(block);
        props.facing = if ctx.world.get_block_state(&target.down()).is_air() {
            ctx.facing
        } else {
            Facing::Up
        };

        // TODO: Carry over items
        let _ = item.split(1);
        ctx.world
            .set_block_state(&target, props.to_state_id(block), BlockFlags::NOTIFY_ALL)
            .await;

        true
    }

    async fn dispense_equipment(ctx: &DispenseContext<'_>, item: &mut ItemStack) -> bool {
        let (slot, allowed_entities, equip_sound) = {
            let Some(equippable) = item.get_data_component::<EquippableImpl>() else {
                return false;
            };
            if !equippable.dispensable {
                return false;
            }
            (
                equippable.slot,
                equippable.allowed_entities.clone(),
                equippable.equip_sound.clone(),
            )
        };

        let target_box = BoundingBox::from_block(&Self::target_position(ctx));
        let players = ctx
            .world
            .get_players_at_box(&target_box)
            .into_iter()
            .map(|player| player as Arc<dyn EntityBase>);

        for entity in ctx
            .world
            .get_entities_at_box(&target_box)
            .into_iter()
            .chain(players)
        {
            let Some(living) = entity.get_living_entity() else {
                continue;
            };
            if !living.is_part_of_game()
                || !is_allowed_entity(allowed_entities.as_ref(), entity.get_entity().entity_type)
            {
                continue;
            }

            let equipment = {
                let mut equipment = living.entity_equipment.lock().await;
                equipment.get_or_insert(slot)
            };
            let mut equipped = equipment.lock().await;
            if !equipped.is_empty() {
                continue;
            }

            let stack = item.split(1);
            living.send_equipment_changes(&[(slot.clone(), stack.clone())]);
            ctx.world.play_sound_event(
                &equip_sound,
                SoundCategory::Blocks,
                &entity.get_entity().pos.load(),
            );
            *equipped = stack;

            return true;
        }

        false
    }

    async fn consume_with_remainder(
        ctx: &DispenseContext<'_>,
        dispenser: &DispenserBlockEntity,
        slot: usize,
        stack: &mut ItemStack,
        remainder: ItemStack,
    ) {
        stack.decrement(1);
        if stack.is_empty() {
            *stack = remainder;
            return;
        }

        for (index, other_slot) in dispenser.items.iter().enumerate() {
            if index == slot {
                continue;
            }

            let mut other = other_slot.lock().await;
            if other.is_empty() {
                *other = remainder;
                return;
            }
            if other.are_items_and_components_equal(&remainder)
                && other.item_count < other.get_max_stack_size()
            {
                other.increment(1);
                return;
            }
        }

        Self::drop_stack(ctx, remainder).await;
    }

    async fn drop_stack(ctx: &DispenseContext<'_>, stack: ItemStack) {
        let facing = to_normal(ctx.facing);
        let mut position = ctx.position.to_centered_f64().add(&(facing * 0.7));

        position.y -= match ctx.facing {
            Facing::Up | Facing::Down => 0.125,
            _ => 0.15625,
        };

        let entity = Entity::new(ctx.world.clone(), position, &EntityType::ITEM);
        let rd = rng().random::<f64>().mul_add(0.1, 0.2);

        let velocity = Vector3::new(
            triangle(&mut rng(), facing.x * rd, 0.017_227_5 * 6.),
            triangle(&mut rng(), 0.2, 0.017_227_5 * 6.),
            triangle(&mut rng(), facing.z * rd, 0.017_227_5 * 6.),
        );

        let item_entity = Arc::new(ItemEntity::new_with_velocity(entity, stack, velocity, 40));
        ctx.world.spawn_entity(item_entity).await;
    }

    async fn drop_at(world: &Arc<World>, position: Vector3<f64>, stack: ItemStack) {
        let entity = Entity::new(world.clone(), position, &EntityType::ITEM);
        world
            .spawn_entity(Arc::new(ItemEntity::new(entity, stack)))
            .await;
    }
}
