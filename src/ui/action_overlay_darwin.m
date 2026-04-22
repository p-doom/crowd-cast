/*
 * Spatial action overlay for predicted next actions.
 *
 * Renders visual indicators (rings for clicks, pills for keyboard actions)
 * on a fullscreen transparent panel. Uses Core Animation (CALayer) for
 * efficient GPU-composited rendering.
 *
 * The panel is excluded from screen capture via SCContentFilter so the
 * inference model never sees its own predictions.
 */

#import <Cocoa/Cocoa.h>
#import <QuartzCore/QuartzCore.h>
#import <ApplicationServices/ApplicationServices.h>

// ---------------------------------------------------------------------------
// Action display item — matches Rust repr(C) layout
// ---------------------------------------------------------------------------

typedef enum : uint32_t {
    ActionKindNone        = 0,
    ActionKindClick       = 1,
    ActionKindDoubleClick = 2,
    ActionKindRightClick  = 3,
    ActionKindDragTo      = 4,
    ActionKindTyping      = 5,
    ActionKindPress       = 6,
    ActionKindHotkey      = 7,
    ActionKindScroll      = 8,
} ActionDisplayKind;

typedef struct {
    ActionDisplayKind kind;
    double screen_x;
    double screen_y;
    uint8_t label[128];
    uint32_t label_len;
} ActionDisplayItem;

// ---------------------------------------------------------------------------
// Globals
// ---------------------------------------------------------------------------

static NSPanel *g_action_panel = nil;
static NSView  *g_content_view = nil;
static uint32_t g_current_item_count = 0;

// Accent color — desaturated cyan
#define ACCENT_R 0.0
#define ACCENT_G 0.898
#define ACCENT_B 1.0

// Click ring
static const CGFloat kRingRadius    = 20.0;
static const CGFloat kRingWidth     = 1.5;
static const CGFloat kGlowInset    = -8.0;    // outer glow extends beyond ring
static const CGFloat kLabelOffset   = 26.0;   // below the ring

// Ghost text
static const CGFloat kGhostMonoSize = 15.0;   // for terminal contexts
static const CGFloat kTabChipH      = 16.0;
static const CGFloat kTabChipPadX   = 6.0;
static const CGFloat kTabChipCorner = 4.0;
static const CGFloat kTabChipGap    = 6.0;    // gap between ghost text and Tab chip

// Pill (fallback)
static const CGFloat kPillHeight    = 32.0;
static const CGFloat kPillPadding   = 16.0;
static const CGFloat kPillBottom    = 60.0;   // from screen bottom
static const CGFloat kPillCorner    = 16.0;   // half of kPillHeight
static const CGFloat kLabelFontSize = 12.0;

// ---------------------------------------------------------------------------
// Accessibility helper — locate focused UI element
// ---------------------------------------------------------------------------

/// Returns the screen rect of the focused UI element, or NSZeroRect if
/// unavailable or the element is too large (e.g. a full window).
static NSRect focused_element_rect(void) {
    AXUIElementRef systemWide = AXUIElementCreateSystemWide();
    AXUIElementRef focusedElement = NULL;

    AXError err = AXUIElementCopyAttributeValue(
        systemWide, kAXFocusedUIElementAttribute, (CFTypeRef *)&focusedElement);
    CFRelease(systemWide);

    if (err != kAXErrorSuccess || focusedElement == NULL) {
        return NSZeroRect;
    }

    CGPoint pos = CGPointZero;
    AXValueRef posValue = NULL;
    AXUIElementCopyAttributeValue(focusedElement, kAXPositionAttribute, (CFTypeRef *)&posValue);
    if (posValue) {
        AXValueGetValue(posValue, (AXValueType)kAXValueCGPointType, &pos);
        CFRelease(posValue);
    }

    CGSize size = CGSizeZero;
    AXValueRef sizeValue = NULL;
    AXUIElementCopyAttributeValue(focusedElement, kAXSizeAttribute, (CFTypeRef *)&sizeValue);
    if (sizeValue) {
        AXValueGetValue(sizeValue, (AXValueType)kAXValueCGSizeType, &size);
        CFRelease(sizeValue);
    }

    CFRelease(focusedElement);

    // Reject elements that are too large (full window) or zero-sized
    if (size.width > 800 || size.height > 200 || size.width < 1 || size.height < 1) {
        return NSZeroRect;
    }

    return NSMakeRect(pos.x, pos.y, size.width, size.height);
}

// ---------------------------------------------------------------------------
// Layer construction helpers
// ---------------------------------------------------------------------------

/// Flip Y from top-left origin (screen capture) to bottom-left (NSWindow).
static CGFloat flip_y(CGFloat y) {
    return NSScreen.mainScreen.frame.size.height - y;
}

/// Create a pulsing ring indicator at the given screen position.
static void add_ring_at(CGFloat screen_x, CGFloat screen_y, NSString *label) {
    CGFloat flipped_y = flip_y(screen_y);
    CGFloat scale = NSScreen.mainScreen.backingScaleFactor;

    // Outer glow (soft halo)
    CGFloat glowRadius = kRingRadius - kGlowInset;
    CGRect glowBounds = CGRectMake(
        screen_x - glowRadius, flipped_y - glowRadius,
        glowRadius * 2, glowRadius * 2);

    CAShapeLayer *glow = [CAShapeLayer layer];
    glow.path = CGPathCreateWithEllipseInRect(
        CGRectMake(0, 0, glowRadius * 2, glowRadius * 2), NULL);
    glow.frame = glowBounds;
    glow.fillColor = [NSColor colorWithRed:ACCENT_R green:ACCENT_G blue:ACCENT_B alpha:0.03].CGColor;
    glow.strokeColor = NULL;
    [g_content_view.layer addSublayer:glow];

    // Inner ring
    CGRect ringBounds = CGRectMake(
        screen_x - kRingRadius, flipped_y - kRingRadius,
        kRingRadius * 2, kRingRadius * 2);

    CAShapeLayer *ring = [CAShapeLayer layer];
    ring.path = CGPathCreateWithEllipseInRect(
        CGRectMake(0, 0, kRingRadius * 2, kRingRadius * 2), NULL);
    ring.frame = ringBounds;
    ring.fillColor = [NSColor colorWithRed:ACCENT_R green:ACCENT_G blue:ACCENT_B alpha:0.06].CGColor;
    ring.strokeColor = [NSColor colorWithRed:ACCENT_R green:ACCENT_G blue:ACCENT_B alpha:0.45].CGColor;
    ring.lineWidth = kRingWidth;

    // Subtle breathing pulse
    CABasicAnimation *pulse = [CABasicAnimation animationWithKeyPath:@"transform.scale"];
    pulse.fromValue = @(0.94);
    pulse.toValue = @(1.06);
    pulse.duration = 1.2;
    pulse.autoreverses = YES;
    pulse.repeatCount = HUGE_VALF;
    pulse.timingFunction = [CAMediaTimingFunction functionWithName:kCAMediaTimingFunctionEaseInEaseOut];
    [ring addAnimation:pulse forKey:@"pulse"];
    [glow addAnimation:pulse forKey:@"pulse"];

    [g_content_view.layer addSublayer:ring];

    // Label below the ring — neutral, unobtrusive
    CATextLayer *text = [CATextLayer layer];
    text.string = label;
    text.font = (__bridge CFTypeRef)[NSFont systemFontOfSize:10.0 weight:NSFontWeightMedium];
    text.fontSize = 10.0;
    text.foregroundColor = [NSColor colorWithWhite:1.0 alpha:0.45].CGColor;
    text.alignmentMode = kCAAlignmentCenter;
    text.contentsScale = scale;

    CGSize textSize = CGSizeMake(140, 14);
    text.frame = CGRectMake(
        screen_x - textSize.width / 2,
        flipped_y - kRingRadius - kLabelOffset - textSize.height,
        textSize.width,
        textSize.height);

    [g_content_view.layer addSublayer:text];
}

/// Create ghost text at the cursor position (like IDE autocomplete).
static void add_ghost_text(CGFloat screen_x, CGFloat screen_y, NSString *text) {
    CGFloat flipped_y = flip_y(screen_y);
    CGFloat scale = NSScreen.mainScreen.backingScaleFactor;

    // Use system monospace font (most predictions are in terminals/code editors)
    NSFont *ghostFont = [NSFont monospacedSystemFontOfSize:kGhostMonoSize weight:NSFontWeightMedium];

    // Measure ghost text width
    NSDictionary *attrs = @{NSFontAttributeName: ghostFont};
    CGSize measured = [text sizeWithAttributes:attrs];
    CGFloat ghostW = ceil(measured.width) + 12;

    // Ghost text layer
    CATextLayer *ghost = [CATextLayer layer];
    ghost.string = text;
    ghost.font = (__bridge CFTypeRef)ghostFont;
    ghost.fontSize = kGhostMonoSize;
    ghost.foregroundColor = [NSColor colorWithWhite:0.5 alpha:1.0].CGColor;
    ghost.contentsScale = scale;
    ghost.alignmentMode = kCAAlignmentLeft;
    ghost.wrapped = NO;
    ghost.truncationMode = kCATruncationNone;

    // Position at cursor — slight vertical offset to align with typical text baselines
    CGFloat textH = ceil(kGhostMonoSize + 6);
    ghost.frame = CGRectMake(
        screen_x,
        flipped_y - textH + 2,  // nudge up to align with baseline
        ghostW,
        textH);

    [g_content_view.layer addSublayer:ghost];

    // "Tab" chip to the right of ghost text
    CGFloat chipTextSize = 9.0;
    NSFont *chipFont = [NSFont systemFontOfSize:chipTextSize weight:NSFontWeightMedium];
    NSString *chipStr = @"\u21E5 Tab";  // ⇥ Tab
    CGSize chipTextMeasured = [chipStr sizeWithAttributes:@{NSFontAttributeName: chipFont}];
    CGFloat chipW = chipTextMeasured.width + kTabChipPadX * 2;
    CGFloat chipX = screen_x + ghostW + kTabChipGap;
    CGFloat chipY = flipped_y - kTabChipH + 2;

    // Chip background
    CGPathRef chipPath = CGPathCreateWithRoundedRect(
        CGRectMake(0, 0, chipW, kTabChipH), kTabChipCorner, kTabChipCorner, NULL);
    CAShapeLayer *chipBg = [CAShapeLayer layer];
    chipBg.path = chipPath;
    chipBg.frame = CGRectMake(chipX, chipY, chipW, kTabChipH);
    chipBg.fillColor = [NSColor colorWithWhite:0.5 alpha:0.15].CGColor;
    chipBg.strokeColor = NULL;
    CGPathRelease(chipPath);
    [g_content_view.layer addSublayer:chipBg];

    // Chip text
    CATextLayer *chipText = [CATextLayer layer];
    chipText.string = chipStr;
    chipText.font = (__bridge CFTypeRef)chipFont;
    chipText.fontSize = chipTextSize;
    chipText.foregroundColor = [NSColor colorWithWhite:0.5 alpha:0.6].CGColor;
    chipText.contentsScale = scale;
    chipText.alignmentMode = kCAAlignmentCenter;
    chipText.frame = CGRectMake(
        chipX + kTabChipPadX,
        chipY + (kTabChipH - chipTextSize - 2) / 2,
        chipTextMeasured.width,
        chipTextSize + 2);
    [g_content_view.layer addSublayer:chipText];
}

/// Create a bottom-center pill with the given label.
static void add_pill(NSString *label) {
    CGFloat screenW = NSScreen.mainScreen.frame.size.width;
    CGFloat scale = NSScreen.mainScreen.backingScaleFactor;

    NSFont *font = [NSFont systemFontOfSize:kLabelFontSize weight:NSFontWeightMedium];
    NSDictionary *attrs = @{NSFontAttributeName: font};
    CGSize textSize = [label sizeWithAttributes:attrs];

    // "Tab" chip + label
    NSString *chipStr = @"\u21E5";
    CGFloat chipFontSize = 9.0;
    NSFont *chipFont = [NSFont systemFontOfSize:chipFontSize weight:NSFontWeightMedium];
    CGSize chipSize = [chipStr sizeWithAttributes:@{NSFontAttributeName: chipFont}];
    CGFloat chipW = chipSize.width + kTabChipPadX * 2;

    CGFloat pillWidth = chipW + 8 + textSize.width + kPillPadding * 2;
    if (pillWidth < 120) pillWidth = 120;

    CGFloat pillX = (screenW - pillWidth) / 2;
    CGFloat pillY = kPillBottom;

    // Pill background
    CGRect pillRect = CGRectMake(0, 0, pillWidth, kPillHeight);
    CGPathRef pillPath = CGPathCreateWithRoundedRect(pillRect, kPillCorner, kPillCorner, NULL);

    CAShapeLayer *bg = [CAShapeLayer layer];
    bg.path = pillPath;
    bg.frame = CGRectMake(pillX, pillY, pillWidth, kPillHeight);
    bg.fillColor = [NSColor colorWithWhite:0.10 alpha:0.80].CGColor;
    bg.strokeColor = [NSColor colorWithWhite:1.0 alpha:0.08].CGColor;
    bg.lineWidth = 0.5;
    CGPathRelease(pillPath);

    // Appear animation
    bg.opacity = 0;
    CABasicAnimation *fadeIn = [CABasicAnimation animationWithKeyPath:@"opacity"];
    fadeIn.fromValue = @(0);
    fadeIn.toValue = @(1);
    fadeIn.duration = 0.2;
    fadeIn.fillMode = kCAFillModeForwards;
    fadeIn.removedOnCompletion = NO;
    fadeIn.timingFunction = [CAMediaTimingFunction functionWithName:kCAMediaTimingFunctionEaseOut];
    [bg addAnimation:fadeIn forKey:@"fadeIn"];

    [g_content_view.layer addSublayer:bg];

    // Tab chip inside pill
    CGFloat chipX = pillX + kPillPadding;
    CGFloat chipY = pillY + (kPillHeight - kTabChipH) / 2;
    CGPathRef chipPath = CGPathCreateWithRoundedRect(
        CGRectMake(0, 0, chipW, kTabChipH), kTabChipCorner, kTabChipCorner, NULL);
    CAShapeLayer *chipBg = [CAShapeLayer layer];
    chipBg.path = chipPath;
    chipBg.frame = CGRectMake(chipX, chipY, chipW, kTabChipH);
    chipBg.fillColor = [NSColor colorWithWhite:1.0 alpha:0.12].CGColor;
    chipBg.strokeColor = NULL;
    CGPathRelease(chipPath);
    [g_content_view.layer addSublayer:chipBg];

    CATextLayer *chipText = [CATextLayer layer];
    chipText.string = chipStr;
    chipText.font = (__bridge CFTypeRef)chipFont;
    chipText.fontSize = chipFontSize;
    chipText.foregroundColor = [NSColor colorWithWhite:1.0 alpha:0.5].CGColor;
    chipText.contentsScale = scale;
    chipText.alignmentMode = kCAAlignmentCenter;
    chipText.frame = CGRectMake(chipX, chipY + (kTabChipH - chipFontSize - 2) / 2, chipW, chipFontSize + 2);
    [g_content_view.layer addSublayer:chipText];

    // Label text
    CATextLayer *text = [CATextLayer layer];
    text.string = label;
    text.font = (__bridge CFTypeRef)font;
    text.fontSize = kLabelFontSize;
    text.foregroundColor = [NSColor colorWithWhite:1.0 alpha:0.85].CGColor;
    text.alignmentMode = kCAAlignmentLeft;
    text.contentsScale = scale;
    text.frame = CGRectMake(
        chipX + chipW + 8,
        pillY + (kPillHeight - kLabelFontSize - 4) / 2,
        textSize.width + 4,
        kLabelFontSize + 4);

    [g_content_view.layer addSublayer:text];
}

/// Show a hint near a focused element rect, or fall back to bottom-center pill.
static void add_element_hint(NSRect elementRect, NSString *label) {
    if (NSEqualRects(elementRect, NSZeroRect)) {
        // Fallback: bottom-center pill
        add_pill(label);
        return;
    }

    // Draw a subtle outline around the focused element
    CGFloat flipped_y = flip_y(elementRect.origin.y + elementRect.size.height);

    CGRect outlineFrame = CGRectMake(
        elementRect.origin.x - 3,
        flipped_y - 3,
        elementRect.size.width + 6,
        elementRect.size.height + 6);

    CGPathRef outlinePath = CGPathCreateWithRoundedRect(
        CGRectMake(0, 0, outlineFrame.size.width, outlineFrame.size.height), 4, 4, NULL);

    CAShapeLayer *outline = [CAShapeLayer layer];
    outline.path = outlinePath;
    outline.frame = outlineFrame;
    outline.fillColor = NULL;
    outline.strokeColor = [NSColor colorWithRed:ACCENT_R green:ACCENT_G blue:ACCENT_B alpha:0.5].CGColor;
    outline.lineWidth = 1.5;
    outline.lineDashPattern = @[@4, @3];
    CGPathRelease(outlinePath);

    [g_content_view.layer addSublayer:outline];

    // Label below the element
    CATextLayer *text = [CATextLayer layer];
    text.string = label;
    text.font = (__bridge CFTypeRef)[NSFont systemFontOfSize:kLabelFontSize weight:NSFontWeightMedium];
    text.fontSize = kLabelFontSize;
    text.foregroundColor = [NSColor colorWithRed:ACCENT_R green:ACCENT_G blue:ACCENT_B alpha:0.9].CGColor;
    text.alignmentMode = kCAAlignmentLeft;
    text.contentsScale = NSScreen.mainScreen.backingScaleFactor;
    text.frame = CGRectMake(
        elementRect.origin.x,
        flipped_y - elementRect.size.height - 20,
        200,
        kLabelFontSize + 4);

    [g_content_view.layer addSublayer:text];
}

/// Remove all indicator sublayers.
static void clear_indicators(void) {
    if (g_content_view == nil) return;
    NSArray *sublayers = [g_content_view.layer.sublayers copy];
    for (CALayer *layer in sublayers) {
        [layer removeFromSuperlayer];
    }
    g_current_item_count = 0;
}

// ---------------------------------------------------------------------------
// Tab-to-execute (moved from overlay_darwin.m)
// ---------------------------------------------------------------------------

typedef void (*ExecuteCallback)(void *context);
static ExecuteCallback g_execute_callback = NULL;
static void *g_execute_context = NULL;
static CFMachPortRef g_tab_tap = NULL;
static CFRunLoopSourceRef g_tab_source = NULL;
static dispatch_queue_t g_execute_queue = NULL;
static _Atomic BOOL g_is_executing = NO;

void action_overlay_set_executing(int executing) {
    g_is_executing = executing ? YES : NO;
}

static CGEventRef tab_tap_callback(CGEventTapProxy proxy, CGEventType type,
                                    CGEventRef event, void *userInfo) {
    (void)proxy; (void)userInfo;

    if (type == kCGEventTapDisabledByTimeout || type == kCGEventTapDisabledByUserInput) {
        if (g_tab_tap) CGEventTapEnable(g_tab_tap, true);
        return event;
    }

    if (type != kCGEventKeyDown) return event;

    int64_t keycode = CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode);
    if (keycode != 48) return event;  // Tab

    CGEventFlags flags = CGEventGetFlags(event);
    CGEventFlags modMask = kCGEventFlagMaskShift | kCGEventFlagMaskControl |
                           kCGEventFlagMaskAlternate | kCGEventFlagMaskCommand;
    if (flags & modMask) return event;

    if (g_is_executing) return event;

    // Only intercept if action overlay is visible and has items
    if (g_action_panel == nil || !g_action_panel.isVisible) return event;
    if (g_current_item_count == 0) return event;

    if (g_execute_callback && g_execute_queue) {
        ExecuteCallback cb = g_execute_callback;
        void *ctx = g_execute_context;
        dispatch_async(g_execute_queue, ^{
            cb(ctx);
        });
    }

    return NULL;  // consume the Tab event
}

static void install_tab_tap(void) {
    if (g_tab_tap != NULL) return;

    CGEventMask mask = (1 << kCGEventKeyDown);
    g_tab_tap = CGEventTapCreate(kCGHIDEventTap, kCGHeadInsertEventTap,
                                  kCGEventTapOptionDefault, mask,
                                  tab_tap_callback, NULL);
    if (!g_tab_tap) {
        fprintf(stderr, "[ActionOverlay] Failed to create CGEventTap for Tab\n");
        return;
    }

    g_tab_source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, g_tab_tap, 0);
    CFRunLoopAddSource(CFRunLoopGetMain(), g_tab_source, kCFRunLoopCommonModes);
    CGEventTapEnable(g_tab_tap, true);
    fprintf(stderr, "[ActionOverlay] Tab event tap installed\n");
}

static void remove_tab_tap(void) {
    if (g_tab_source) {
        CFRunLoopRemoveSource(CFRunLoopGetMain(), g_tab_source, kCFRunLoopCommonModes);
        CFRelease(g_tab_source);
        g_tab_source = NULL;
    }
    if (g_tab_tap) {
        CGEventTapEnable(g_tab_tap, false);
        CFRelease(g_tab_tap);
        g_tab_tap = NULL;
    }
    fprintf(stderr, "[ActionOverlay] Tab event tap removed\n");
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

void action_overlay_create(void) {
    void (^block)(void) = ^{
        if (g_action_panel != nil) return;

        NSRect screenFrame = [[NSScreen mainScreen] frame];

        g_action_panel = [[NSPanel alloc]
            initWithContentRect:screenFrame
                      styleMask:NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel
                        backing:NSBackingStoreBuffered
                          defer:NO];

        g_action_panel.level = NSStatusWindowLevel + 1;
        g_action_panel.floatingPanel = YES;
        g_action_panel.becomesKeyOnlyIfNeeded = YES;
        g_action_panel.hidesOnDeactivate = NO;
        g_action_panel.collectionBehavior =
            NSWindowCollectionBehaviorCanJoinAllSpaces |
            NSWindowCollectionBehaviorStationary |
            NSWindowCollectionBehaviorFullScreenAuxiliary;
        g_action_panel.backgroundColor = [NSColor clearColor];
        g_action_panel.opaque = NO;
        g_action_panel.hasShadow = NO;
        g_action_panel.ignoresMouseEvents = YES;

        g_content_view = [[NSView alloc] initWithFrame:screenFrame];
        g_content_view.wantsLayer = YES;
        g_content_view.layer.backgroundColor = [NSColor clearColor].CGColor;
        g_action_panel.contentView = g_content_view;

        [g_action_panel orderFront:nil];
    };

    if ([NSThread isMainThread]) {
        block();
    } else {
        dispatch_async(dispatch_get_main_queue(), block);
    }
}

void action_overlay_update(const ActionDisplayItem *items, uint32_t count) {
    if (items == NULL || count == 0) return;

    // Copy items for the block
    ActionDisplayItem *copied = malloc(sizeof(ActionDisplayItem) * count);
    memcpy(copied, items, sizeof(ActionDisplayItem) * count);
    uint32_t n = count;

    dispatch_async(dispatch_get_main_queue(), ^{
        if (g_content_view == nil || g_action_panel == nil) {
            free(copied);
            return;
        }

        clear_indicators();

        for (uint32_t i = 0; i < n; i++) {
            ActionDisplayItem item = copied[i];
            NSString *label = [[NSString alloc]
                initWithBytes:item.label
                       length:item.label_len
                     encoding:NSUTF8StringEncoding];
            if (!label) label = @"Tab";

            BOOL has_coordinate = (item.screen_x != 0.0 || item.screen_y != 0.0);

            switch (item.kind) {
                case ActionKindClick:
                case ActionKindDoubleClick:
                case ActionKindRightClick:
                case ActionKindDragTo:
                    add_ring_at(item.screen_x, item.screen_y, label);
                    break;

                case ActionKindTyping: {
                    // Extract the raw text from label (strip "Tab: type '" prefix and "'" suffix)
                    NSString *rawText = label;
                    if ([label hasPrefix:@"Tab: type '"]) {
                        rawText = [label substringFromIndex:11];
                        if ([rawText hasSuffix:@"'"]) {
                            rawText = [rawText substringToIndex:rawText.length - 1];
                        }
                    }
                    if (has_coordinate) {
                        add_ghost_text(item.screen_x, item.screen_y, rawText);
                    } else {
                        NSRect elementRect = focused_element_rect();
                        if (!NSEqualRects(elementRect, NSZeroRect)) {
                            // Show ghost text at end of focused element
                            CGFloat ex = elementRect.origin.x + elementRect.size.width;
                            CGFloat ey = elementRect.origin.y + elementRect.size.height / 2;
                            add_ghost_text(ex, ey, rawText);
                        } else {
                            add_pill(label);
                        }
                    }
                    break;
                }

                case ActionKindPress:
                case ActionKindHotkey:
                case ActionKindScroll: {
                    if (has_coordinate) {
                        add_ring_at(item.screen_x, item.screen_y, label);
                    } else {
                        add_pill(label);
                    }
                    break;
                }

                default:
                    break;
            }
        }

        g_current_item_count = n;
        free(copied);
    });
}

void action_overlay_clear(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        clear_indicators();
    });
}

void action_overlay_set_visible(int visible) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (g_action_panel == nil) return;
        if (visible) {
            [g_action_panel orderFront:nil];
            if (g_execute_callback) install_tab_tap();
        } else {
            [g_action_panel orderOut:nil];
            remove_tab_tap();
        }
    });
}

uint32_t action_overlay_get_window_id(void) {
    if (g_action_panel == nil) return 0;
    return (uint32_t)[g_action_panel windowNumber];
}

void action_overlay_register_execute_callback(ExecuteCallback callback, void *context) {
    g_execute_callback = callback;
    g_execute_context = context;
    if (!g_execute_queue) {
        g_execute_queue = dispatch_queue_create("dev.crowd-cowork.action-execute",
                                                 DISPATCH_QUEUE_SERIAL);
    }
    dispatch_async(dispatch_get_main_queue(), ^{
        if (g_action_panel != nil && g_action_panel.isVisible) {
            install_tab_tap();
        }
    });
}

void action_overlay_destroy(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        remove_tab_tap();
        if (g_action_panel != nil) {
            [g_action_panel close];
            g_action_panel = nil;
            g_content_view = nil;
        }
    });
}
