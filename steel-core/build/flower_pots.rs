use std::collections::BTreeSet;

use quote::quote;

use crate::{blocks::BlockClass, to_block_ident};

pub fn build(blocks: &[BlockClass]) -> String {
    let flower_pots: Vec<_> = blocks
        .iter()
        .filter(|block| block.class == "FlowerPotBlock")
        .map(|block| {
            let potted = block
                .extra
                .get("potted")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| {
                    panic!(
                        "FlowerPotBlock '{}' is missing its extracted string 'potted' field",
                        block.name
                    )
                });
            (block.name.as_str(), potted)
        })
        .collect();

    assert!(
        !flower_pots.is_empty(),
        "classes.json has no FlowerPotBlock entries"
    );

    let mut pot_names = BTreeSet::new();
    let mut content_names = BTreeSet::new();
    let mut empty_pot_count = 0;
    for &(pot, content) in &flower_pots {
        assert!(
            pot_names.insert(pot),
            "duplicate extracted flower pot '{pot}'"
        );
        assert!(
            content_names.insert(content),
            "duplicate extracted flower-pot content '{content}'"
        );
        empty_pot_count += usize::from(content == "air");
    }
    assert_eq!(
        empty_pot_count, 1,
        "expected exactly one extracted flower pot whose content is air"
    );

    let entries = flower_pots.iter().map(|&(pot, content)| {
        let pot = to_block_ident(pot);
        let content = to_block_ident(content);
        quote! { (&vanilla_blocks::#pot, &vanilla_blocks::#content) }
    });
    let item_to_pot = flower_pots
        .iter()
        .filter(|&&(_, content)| content != "air")
        .map(|&(pot, content)| {
            let pot = to_block_ident(pot);
            let content = to_block_ident(content);
            quote! {
                candidate if candidate == REGISTRY.items.by_block(&vanilla_blocks::#content) => {
                    Some(&vanilla_blocks::#pot)
                }
            }
        });
    let entry_count = flower_pots.len();

    quote! {
        //! Generated flower-pot relationships extracted from Vanilla's `FlowerPotBlock` instances.

        use steel_registry::{REGISTRY, blocks::BlockRef, items::ItemRef, vanilla_blocks};

        /// Number of extracted empty and occupied flower-pot blocks.
        pub const FLOWER_POT_ENTRY_COUNT: usize = #entry_count;

        /// Returns every extracted `(flower pot, content)` relationship.
        #[must_use]
        pub fn entries() -> [(BlockRef, BlockRef); FLOWER_POT_ENTRY_COUNT] {
            [#(#entries),*]
        }

        /// Returns the occupied flower pot produced by inserting this block item.
        #[must_use]
        pub fn by_item(item: ItemRef) -> Option<BlockRef> {
            match item {
                #(#item_to_pot,)*
                _ => None,
            }
        }
    }
    .to_string()
}
