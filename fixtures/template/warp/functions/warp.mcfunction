UNPACK:SUBFUNCTION

definefunction <setup>:
    # Prepare the warp
    scoreboard objectives add warp dummy
    for <i 0..3>:
        scoreboard players set point_`eval:i` warp `eval:i`
    execute as @a run function <reset>:
        tag @s remove warping
        ## dropped
        tag @s add warp_ready

definefunction <tick>:
    execute as @a[tag=warping] run function warp/setup/reset
    schedule on_area_loaded add 0 0 0 1 1 1 <later>:
        say loaded

definefunction <nested/deep>:
    say deep
