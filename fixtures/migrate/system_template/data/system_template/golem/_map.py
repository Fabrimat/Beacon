[
    {
        "source": "golem.behavior.json",
        "target": AUTO,
        "json_template": True,
        "scope": {"health": config_health}
    },
    {
        "source": "sub/golem.entity.json",
        "target": AUTO_FLAT
    },
    {
        "source": "sub/tree.feature.json",
        "target": AUTO
    },
    {
        "source": "paint.item.json",
        "target": "BP/items/paint.json",
        "json_template": True
    },
    {
        "source": "en_US.lang",
        "target": AUTO
    },
    {
        "source": "item_texture.json",
        "target": "RP/textures/item_texture.json"
    },
    {
        "source": "golem.recipe.json",
        "target": "BP/recipes/golem.json"
    },
    {
        "source": "shared.item.json",
        "target": AUTO_FLAT
    },
    {
        "source": "notes.txt",
        "target": "data/st_notes/golem.txt"
    }
] + [
    {
        "source": "spawn_rule.json",
        "target": f"BP/spawn_rules/golem_{c}.json",
        "json_template": True,
        "scope": {"color": c}
    }
    for c in colors
]
