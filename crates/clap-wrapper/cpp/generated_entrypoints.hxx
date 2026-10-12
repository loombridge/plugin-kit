// The Audio Unit entry point for the one plug-in in a kit library, in place of
// the file clap-wrapper's CMake build generates. The Audio Unit's type (an
// instrument, an effect or a note effect) is read from the component
// description the host found in the bundle's Info.plist, so one entry serves
// every kind of plug-in.
//
// Adapted from clap-wrapper-rs (https://github.com/blepfx/clap-wrapper-rs),
// MIT OR Apache-2.0.

#pragma once
#include "detail/auv2/auv2_base_classes.h"

static AUV2_Type lspkTypeFromInstance(AudioComponentInstance ci)
{
  AudioComponent comp = AudioComponentInstanceGetComponent(ci);
  AudioComponentDescription desc;
  if (comp == nullptr || AudioComponentGetDescription(comp, &desc) != 0)
    return AUV2_Type::aumu_musicdevice;
  if (desc.componentType == 'aufx') return AUV2_Type::aufx_effect;
  if (desc.componentType == 'aumi') return AUV2_Type::aumi_noteeffect;
  return AUV2_Type::aumu_musicdevice;
}

struct lspkWrapAsAUV2 : free_audio::auv2_wrapper::WrapAsAUV2
{
  lspkWrapAsAUV2(AudioComponentInstance ci)
    : free_audio::auv2_wrapper::WrapAsAUV2(lspkTypeFromInstance(ci), "", "", 0, ci)
  {
  }
};
AUSDK_COMPONENT_ENTRY(ausdk::AUMusicDeviceFactory, lspkWrapAsAUV2);
