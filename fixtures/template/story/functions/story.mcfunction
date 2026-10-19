## The story beats (one function per item)
foreach <index item items>:
    var name = item ['identifier']
    execute if score STORY var matches `eval:index` run execute as @e[type=demo:guide,c=1] run function <`eval:name`>:
        dialogue open @s @p guide_`eval:name`   
        event entity @s interact_with_`eval:name`
    tag @p[tag=!HAS_ITEM_`eval:index`] add HAS_ITEM_`eval:index`       
scoreboard players set GAME_STATE var `eval:game_state['lobby']`
tp @s `eval:area['center']['x']` `eval:area['center']['y']` `eval:area['center']['z']`
say `eval:area['radius'] * 2` `eval:area['enabled']` `eval:flag` `eval:'v' + str_part`
