# The vcpkg port: the tree at the release's commit, built by cargo, the static library and the headers installed.
# packages/build.sh fills @REF@; the port goes to the vcpkg registry, or to a project as an overlay port.
vcpkg_from_git(OUT_SOURCE_PATH SOURCE_PATH URL https://github.com/jacob-mellor/mudRPC REF @REF@)
find_program(CARGO cargo REQUIRED)
set(TARGET_DIR "${CURRENT_BUILDTREES_DIR}/${TARGET_TRIPLET}/cargo")
vcpkg_execute_required_process(COMMAND "${CARGO}" build --locked --release -p mudrpc-c --target-dir "${TARGET_DIR}" WORKING_DIRECTORY "${SOURCE_PATH}" LOGNAME "cargo-${TARGET_TRIPLET}")
file(INSTALL "${TARGET_DIR}/release/libmudrpc.a" DESTINATION "${CURRENT_PACKAGES_DIR}/lib")
if(NOT VCPKG_BUILD_TYPE)
    file(INSTALL "${TARGET_DIR}/release/libmudrpc.a" DESTINATION "${CURRENT_PACKAGES_DIR}/debug/lib")   # one build, overflow checks on, serves both
endif()
file(INSTALL "${SOURCE_PATH}/bindings/c/mudrpc.h" "${SOURCE_PATH}/bindings/cpp/mud.hpp" "${SOURCE_PATH}/bindings/cpp/mud_grpc.hpp" DESTINATION "${CURRENT_PACKAGES_DIR}/include")
file(INSTALL "${SOURCE_PATH}/bindings/cpp/SKILL.md" DESTINATION "${CURRENT_PACKAGES_DIR}/share/${PORT}")
vcpkg_install_copyright(FILE_LIST "${SOURCE_PATH}/LICENSE")
