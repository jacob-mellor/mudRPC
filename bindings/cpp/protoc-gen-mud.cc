// protoc-gen-mud: gRPC's C++ Stub and Service for every service in a .proto, over mudRPC. Adapted from gRPC's C++ plugin,
// src/compiler/cpp_generator.cc (Copyright 2015 gRPC authors, Apache License 2.0): its sync API alone, written to one
// header, X.mud.pb.h beside protoc's X.pb.h, with ::mud:: where gRPC's has ::grpc::.
//
//   c++ -std=c++17 protoc-gen-mud.cc $(pkg-config --cflags --libs protobuf) -lprotoc -o protoc-gen-mud
//   protoc --plugin=protoc-gen-mud=./protoc-gen-mud --cpp_out=. --mud_out=. drone.proto
#include <google/protobuf/compiler/code_generator.h>
#include <google/protobuf/compiler/plugin.h>
#include <google/protobuf/descriptor.h>
#include <google/protobuf/io/coded_stream.h>
#include <google/protobuf/io/zero_copy_stream.h>
#include <google/protobuf/stubs/common.h>

#include <algorithm>
#include <cctype>
#include <map>
#include <memory>
#include <string>

namespace {
using google::protobuf::Descriptor;
using google::protobuf::FileDescriptor;
using google::protobuf::MethodDescriptor;
using google::protobuf::ServiceDescriptor;
using Vars = std::map<std::string, std::string>;

// gRPC's Printer, $name$ replaced by its value.
void Print(std::string* out, const Vars& vars, const std::string& text) {
  for (size_t i = 0; i < text.size(); i++) {
    size_t j = text[i] == '$' ? text.find('$', i + 1) : std::string::npos;
    if (j == std::string::npos) out->push_back(text[i]);
    else {
      out->append(vars.at(text.substr(i + 1, j - i - 1)));
      i = j;
    }
  }
}

std::string Replace(std::string s, const std::string& from, const std::string& to) {
  for (size_t i = s.find(from); i != std::string::npos; i = s.find(from, i + to.size())) s.replace(i, from.size(), to);
  return s;
}

// gRPC's ClassName, qualified: the C++ class protoc's --cpp_out names a message.
std::string ClassName(const Descriptor* d) {
  const Descriptor* outer = d;
  while (outer->containing_type()) outer = outer->containing_type();
  std::string o(outer->full_name()), full(d->full_name());
  return "::" + Replace(o, ".", "::") + Replace(full.substr(o.size()), ".", "_");
}

// gRPC's FilenameIdentifier, for the include guard.
std::string FilenameIdentifier(const std::string& name) {
  std::string out;
  for (char c : name) {
    if (isalnum(static_cast<unsigned char>(c))) out.push_back(c);
    else {
      static const char hex[] = "0123456789abcdef";
      out += {'_', hex[(c >> 4) & 0xf], hex[c & 0xf]};
    }
  }
  return out;
}

enum Shape { kUnary, kServer, kClient, kBidi };
Shape ShapeOf(const MethodDescriptor* m) { return m->client_streaming() ? (m->server_streaming() ? kBidi : kClient) : (m->server_streaming() ? kServer : kUnary); }

void PrintService(std::string* out, const ServiceDescriptor* service, const std::string& package) {
  Vars vars{{"Service", std::string(service->name())}, {"full", package + std::string(service->name())}};
  auto each = [&](const std::map<Shape, std::string>& text) {
    for (int i = 0; i < service->method_count(); i++) {
      const MethodDescriptor* m = service->method(i);
      vars["Method"] = std::string(m->name());
      vars["Request"] = ClassName(m->input_type());
      vars["Response"] = ClassName(m->output_type());
      auto t = text.find(ShapeOf(m));
      if (t != text.end()) Print(out, vars, t->second);
    }
  };
  Print(out, vars,
        "class $Service$ final {\n"
        " public:\n"
        "  static constexpr char const* service_full_name() { return \"$full$\"; }\n\n"
        "  class StubInterface {\n"
        "   public:\n"
        "    virtual ~StubInterface() {}\n");
  each({{kUnary, "    virtual ::mud::Status $Method$(::mud::ClientContext* context, const $Request$& request, $Response$* response) = 0;\n"},
        {kServer, "    std::unique_ptr< ::mud::ClientReaderInterface< $Response$>> $Method$(::mud::ClientContext* context, const $Request$& request) {\n"
                  "      return std::unique_ptr< ::mud::ClientReaderInterface< $Response$>>($Method$Raw(context, request));\n    }\n"},
        {kClient, "    std::unique_ptr< ::mud::ClientWriterInterface< $Request$>> $Method$(::mud::ClientContext* context, $Response$* response) {\n"
                  "      return std::unique_ptr< ::mud::ClientWriterInterface< $Request$>>($Method$Raw(context, response));\n    }\n"},
        {kBidi, "    std::unique_ptr< ::mud::ClientReaderWriterInterface< $Request$, $Response$>> $Method$(::mud::ClientContext* context) {\n"
                "      return std::unique_ptr< ::mud::ClientReaderWriterInterface< $Request$, $Response$>>($Method$Raw(context));\n    }\n"}});
  Print(out, vars, "\n   private:\n");
  each({{kServer, "    virtual ::mud::ClientReaderInterface< $Response$>* $Method$Raw(::mud::ClientContext* context, const $Request$& request) = 0;\n"},
        {kClient, "    virtual ::mud::ClientWriterInterface< $Request$>* $Method$Raw(::mud::ClientContext* context, $Response$* response) = 0;\n"},
        {kBidi, "    virtual ::mud::ClientReaderWriterInterface< $Request$, $Response$>* $Method$Raw(::mud::ClientContext* context) = 0;\n"}});
  Print(out, vars,
        "  };\n\n"
        "  class Stub final : public StubInterface {\n"
        "   public:\n"
        "    Stub(const std::shared_ptr< ::mud::ChannelInterface>& channel, const ::mud::StubOptions& options = ::mud::StubOptions()) : channel_(channel) { (void)options; }\n");
  each({{kUnary, "    ::mud::Status $Method$(::mud::ClientContext* context, const $Request$& request, $Response$* response) override {\n"
                 "      return ::mud::internal::BlockingUnaryCall(channel_.get(), \"$full$\", \"$Method$\", context, request, response);\n    }\n"},
        {kServer, "    std::unique_ptr< ::mud::ClientReader< $Response$>> $Method$(::mud::ClientContext* context, const $Request$& request) {\n"
                  "      return std::unique_ptr< ::mud::ClientReader< $Response$>>($Method$Raw(context, request));\n    }\n"},
        {kClient, "    std::unique_ptr< ::mud::ClientWriter< $Request$>> $Method$(::mud::ClientContext* context, $Response$* response) {\n"
                  "      return std::unique_ptr< ::mud::ClientWriter< $Request$>>($Method$Raw(context, response));\n    }\n"},
        {kBidi, "    std::unique_ptr< ::mud::ClientReaderWriter< $Request$, $Response$>> $Method$(::mud::ClientContext* context) {\n"
                "      return std::unique_ptr< ::mud::ClientReaderWriter< $Request$, $Response$>>($Method$Raw(context));\n    }\n"}});
  Print(out, vars, "\n   private:\n    std::shared_ptr< ::mud::ChannelInterface> channel_;\n");
  each({{kServer, "    ::mud::ClientReader< $Response$>* $Method$Raw(::mud::ClientContext* context, const $Request$& request) override {\n"
                  "      return new ::mud::ClientReader< $Response$>(channel_.get(), \"$full$\", \"$Method$\", context, request);\n    }\n"},
        {kClient, "    ::mud::ClientWriter< $Request$>* $Method$Raw(::mud::ClientContext*, $Response$*) override { return new ::mud::ClientWriter< $Request$>(); }\n"},
        {kBidi, "    ::mud::ClientReaderWriter< $Request$, $Response$>* $Method$Raw(::mud::ClientContext*) override { return new ::mud::ClientReaderWriter< $Request$, $Response$>(); }\n"}});
  Print(out, vars,
        "  };\n\n"
        "  static std::unique_ptr<Stub> NewStub(const std::shared_ptr< ::mud::ChannelInterface>& channel, const ::mud::StubOptions& options = ::mud::StubOptions()) {\n"
        "    return std::unique_ptr<Stub>(new Stub(channel, options));\n  }\n\n"
        "  class Service : public ::mud::Service {\n"
        "   public:\n"
        "    Service() : ::mud::Service(\"$full$\") {\n");
  each({{kUnary, "      AddMethod(\"$Method$\", ::mud::internal::Unary< $Request$, $Response$>([this](::mud::ServerContext* context, const $Request$* request, $Response$* response) { return $Method$(context, request, response); }));\n"},
        {kServer, "      AddMethod(\"$Method$\", ::mud::internal::ServerStreaming< $Request$, $Response$>([this](::mud::ServerContext* context, const $Request$* request, ::mud::ServerWriter< $Response$>* writer) { return $Method$(context, request, writer); }));\n"},
        {kClient, "      AddMethod(\"$Method$\", ::mud::internal::ClientStreaming());\n"},
        {kBidi, "      AddMethod(\"$Method$\", ::mud::internal::ClientStreaming());\n"}});
  Print(out, vars, "    }\n    virtual ~Service() {}\n");
  each({{kUnary, "    virtual ::mud::Status $Method$(::mud::ServerContext*, const $Request$*, $Response$*) { return ::mud::Status(::mud::StatusCode::UNIMPLEMENTED, \"\"); }\n"},
        {kServer, "    virtual ::mud::Status $Method$(::mud::ServerContext*, const $Request$*, ::mud::ServerWriter< $Response$>*) { return ::mud::Status(::mud::StatusCode::UNIMPLEMENTED, \"\"); }\n"},
        {kClient, "    virtual ::mud::Status $Method$(::mud::ServerContext*, ::mud::ServerReader< $Request$>*, $Response$*) { return ::mud::Status(::mud::StatusCode::UNIMPLEMENTED, \"\"); }\n"},
        {kBidi, "    virtual ::mud::Status $Method$(::mud::ServerContext*, ::mud::ServerReaderWriter< $Response$, $Request$>*) { return ::mud::Status(::mud::StatusCode::UNIMPLEMENTED, \"\"); }\n"}});
  Print(out, vars, "  };\n};\n\n");
}

class Generator : public google::protobuf::compiler::CodeGenerator {
 public:
#if GOOGLE_PROTOBUF_VERSION >= 5027000   // editions, from protobuf 27
  uint64_t GetSupportedFeatures() const override { return FEATURE_PROTO3_OPTIONAL | FEATURE_SUPPORTS_EDITIONS; }
  google::protobuf::Edition GetMinimumEdition() const override { return google::protobuf::Edition::EDITION_PROTO2; }
  google::protobuf::Edition GetMaximumEdition() const override { return google::protobuf::Edition::EDITION_2023; }
#else
  uint64_t GetSupportedFeatures() const override { return FEATURE_PROTO3_OPTIONAL; }
#endif

  bool Generate(const FileDescriptor* file, const std::string&, google::protobuf::compiler::GeneratorContext* context, std::string*) const override {
    std::string name(file->name()), package(file->package()), out;
    std::string base = name.size() > 6 && name.compare(name.size() - 6, 6, ".proto") == 0 ? name.substr(0, name.size() - 6) : name;
    Vars vars{{"file", name}, {"guard", "MUD_" + FilenameIdentifier(name) + "__INCLUDED"}, {"base", base}};
    Print(&out, vars,
          "// Generated by protoc-gen-mud from $file$: gRPC's C++ Stub and Service over mudRPC. Do not edit.\n"
          "#ifndef $guard$\n#define $guard$\n\n#include \"$base$.pb.h\"\n#include \"mud_grpc.hpp\"\n\n");
    std::string close;
    for (size_t i = 0, j; i < package.size(); i = j + 1) {
      j = std::min(package.find('.', i), package.size());
      vars["part"] = package.substr(i, j - i);
      Print(&out, vars, "namespace $part$ {\n");
      close = "}  // namespace " + vars["part"] + "\n" + close;
    }
    out += "\n";
    for (int i = 0; i < file->service_count(); i++) PrintService(&out, file->service(i), package.empty() ? "" : package + ".");
    Print(&out, vars, close + "\n#endif  // $guard$\n");
    std::unique_ptr<google::protobuf::io::ZeroCopyOutputStream> stream(context->Open(base + ".mud.pb.h"));
    google::protobuf::io::CodedOutputStream(stream.get()).WriteRaw(out.data(), static_cast<int>(out.size()));
    return true;
  }
};
}  // namespace

int main(int argc, char* argv[]) {
  Generator generator;
  return google::protobuf::compiler::PluginMain(argc, argv, &generator);
}
