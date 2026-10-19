#!/usr/bin/env nu
# Regenerates the README screenshots in docs/images from tests/fixtures.
# Needs Google Chrome; set $env.CHROME to use another Chromium binary.

const REPO = path self ..
const IMAGES = 'docs/images'
const DEFAULT_CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'

def main [] {
    cd $REPO
    let tmp = mktemp --directory
    try {
        generate ($env.CHROME? | default $DEFAULT_CHROME) $tmp
    } finally {
        rm --recursive --force $tmp
    }
    ls $IMAGES | select name size
}

def generate [chrome: string, tmp: path] {
    mkdir $IMAGES
    ^cargo build --release --quiet --example term2html
    ^cargo build --release --quiet
    let ansiopt = 'target/release/ansiopt'

    # The terminal views, as terminal windows on a transparent background.
    let diff = ^$ansiopt --diff --color always tests/fixtures/rg.ansi | lines
    [...($diff | first 14) $"(ansi attr_dimmed)    ⋮(ansi reset)" ...($diff | last 7)]
    | str join "\n"
    | terminal $chrome $tmp diff '> ansiopt --diff tests/fixtures/rg.ansi'

    ^$ansiopt --explain --color always tests/fixtures/vim.ansi
    | lines
    | first 18
    | str join "\n"
    | terminal $chrome $tmp explain '> ansiopt --explain tests/fixtures/vim.ansi' --columns 126

    ^$ansiopt --hotspot --color always tests/fixtures/bat.ansi
    | lines
    | first 24
    | str join "\n"
    | terminal $chrome $tmp hotspot '> ansiopt --hotspot tests/fixtures/bat.ansi'

    # The HTML report, one tab at a time, in both color schemes. Only the
    # first keeps the page header and summary tiles; the others use a copy of
    # the page with them hidden.
    let report = $tmp | path join report.html
    ^$ansiopt --html $report tests/fixtures/bat.ansi e>| ignore
    let tabs_page = $tmp | path join tabs.html
    open --raw $report
    | str replace '</head>' '<style>header, .tiles { display: none; } main { padding-top: 8px; }</style></head>'
    | save $tabs_page

    let tabs = [[name height]; [diff 900] [rendered 700] [breakdown 400] [changes 400]]
    let schemes = [[theme preference]; [light 1] [dark 0]]
    for tab in $tabs {
        let page = if $tab.name == 'diff' { $report } else { $tabs_page }
        for scheme in $schemes {
            let png = $env.PWD | path join $IMAGES $'report-($tab.name)-($scheme.theme).png'
            (shot $chrome $'($page)#tab-($tab.name)' $png 1100 $tab.height
                $'--blink-settings=preferredColorScheme=($scheme.preference)')
        }
    }
}

# Renders terminal output as a terminal window and screenshots it.
def terminal [
    chrome: string
    tmp: path
    name: string
    title: string
    --columns: int = 160 # lines longer than this wrap
]: string -> nothing {
    let rendered = $in | ^target/release/examples/term2html $title ($columns | into string) | complete
    if $rendered.exit_code != 0 {
        error make {msg: $'term2html failed: ($rendered.stderr)'}
    }
    let page = $tmp | path join $'($name).html'
    $rendered.stdout | save --force $page
    let size = $rendered.stderr | str trim | split row ' ' | into int
    let png = $env.PWD | path join $IMAGES $'($name).png'
    shot $chrome $page $png $size.0 $size.1 '--default-background-color=00000000'
}

# Screenshots a page at twice the given size in CSS pixels.
def shot [
    chrome: string
    page: string
    png: path
    width: int
    height: int
    ...flags: string
] {
    (^$chrome --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=2
        $'--window-size=($width),($height)' $'--screenshot=($png)' ...$flags $'file://($page)')
    | complete
    | ignore
}
