[
    {
        "source": "glob/*.item.json",
        "target": AUTO_FLAT
    },
    {
        "source": "en_US.lang",
        "target": AUTO
    },
    {
        "source": "item_texture.json",
        "target": "RP/textures/item_texture.json",
        "on_conflict": "merge"
    },
    {
        "source": "golem.recipe.json",
        "target": "BP/recipes/golem.json",
        "on_conflict": "overwrite"
    },
    {
        "source": "shared.item.json",
        "target": AUTO_FLAT,
        "on_conflict": "skip"
    }
]
