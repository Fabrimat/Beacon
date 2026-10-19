execute if score STORY var matches 0 run execute as @e[type=demo:guide,c=1] run function story/story/apple
tag @p[tag=!HAS_ITEM_0] add HAS_ITEM_0
execute if score STORY var matches 1 run execute as @e[type=demo:guide,c=1] run function story/story/bread
tag @p[tag=!HAS_ITEM_1] add HAS_ITEM_1
execute if score STORY var matches 2 run execute as @e[type=demo:guide,c=1] run function story/story/carrot
tag @p[tag=!HAS_ITEM_2] add HAS_ITEM_2
scoreboard players set GAME_STATE var 1
tp @s 10.5 64 -3.25
say 5.0 True False v2