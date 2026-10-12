// C entry points for src/main.rs: the validator's own main (or wmain, on
// Windows), which the build script renames, has C++ linkage.

#ifdef _WIN32
int lspk_vst3_validator_wmain(int argc, wchar_t* argv[]);
extern "C" int lspk_vst3_validator_run_wide(int argc, wchar_t* argv[])
{
	return lspk_vst3_validator_wmain(argc, argv);
}
#else
int lspk_vst3_validator_main(int argc, char* argv[]);
extern "C" int lspk_vst3_validator_run(int argc, char* argv[])
{
	return lspk_vst3_validator_main(argc, argv);
}
#endif
