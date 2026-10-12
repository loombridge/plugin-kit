// The Cocoa view classes for the Audio Unit's interface, in place of the file
// clap-wrapper's CMake build generates. The build script gives the classes
// names of their own (CLAP_WRAPPER_COCOA_CLASS and
// CLAP_WRAPPER_COCOA_CLASS_NSVIEW).
//
// Adapted from clap-wrapper-rs (https://github.com/blepfx/clap-wrapper-rs),
// MIT OR Apache-2.0.

#pragma once

#define CLAP_WRAPPER_TIMER_CALLBACK lspk_timerCallback_cocoaUI
#define CLAP_WRAPPER_FILL_AUCV lspk_fillAUCV_cocoaUI
#define CLAP_WRAPPER_EDITOR_NAME "Plugin View"
#include "detail/auv2/wrappedview.asinclude.mm"

bool fillAudioUnitCocoaView(AudioUnitCocoaViewInfo *viewInfo, std::shared_ptr<Clap::Plugin> _plugin)
{
  return lspk_fillAUCV_cocoaUI(viewInfo);
}
