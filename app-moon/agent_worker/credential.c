#include <moonbit.h>

#ifdef _WIN32
#include <windows.h>
#include <wincred.h>
#include <string.h>
#pragma comment(lib, "Advapi32.lib")

MOONBIT_FFI_EXPORT
moonbit_string_t astock_read_minimax_key(void) {
  static const wchar_t target[] = L"minimax-api-key.astock-terminal";
  PCREDENTIALW credential = NULL;
  if (!CredReadW(target, CRED_TYPE_GENERIC, 0, &credential) || credential == NULL) {
    return moonbit_make_string_raw(0);
  }
  DWORD bytes = credential->CredentialBlobSize;
  if (bytes == 0 || (bytes % sizeof(wchar_t)) != 0 || credential->CredentialBlob == NULL) {
    CredFree(credential);
    return moonbit_make_string_raw(0);
  }
  size_t chars = bytes / sizeof(wchar_t);
  moonbit_string_t result = moonbit_make_string_raw((int32_t)chars);
  memcpy(result, credential->CredentialBlob, bytes);
  SecureZeroMemory(credential->CredentialBlob, bytes);
  CredFree(credential);
  return result;
}
#else
MOONBIT_FFI_EXPORT
moonbit_string_t astock_read_minimax_key(void) {
  return moonbit_make_string_raw(0);
}
#endif
