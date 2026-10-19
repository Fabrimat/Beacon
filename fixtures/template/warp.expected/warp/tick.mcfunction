execute as @a[tag=warping] run function warp/setup/reset
schedule on_area_loaded add 0 0 0 1 1 1 warp/warp/tick/later