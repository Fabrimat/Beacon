UNPACK:SUBFUNCTION

definefunction <update>:
    # Sets the XP bar to XP_PROGRESS (0..100).

    ## level = (1000 + 158) / 9, rounded up
    xp -1000L
    xp 129L
    for <i 0..101>:
        execute if score XP_PROGRESS var matches `eval:i` run xp `eval:10*i`

    xp -129L
