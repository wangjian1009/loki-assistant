import Foundation

protocol EmacsCommunicating: Sendable {
    func ping(configuration: EmacsConfiguration) async throws -> ConnectionInfo
    func listSessions(configuration: EmacsConfiguration) async throws -> SessionList
    func selectSession(configuration: EmacsConfiguration, sessionID: String) async throws -> SelectedSession
    func sendPrompt(configuration: EmacsConfiguration, sessionID: String, prompt: String) async throws -> AcceptedPrompt
}

struct EmacsClient: EmacsCommunicating {
    func ping(configuration: EmacsConfiguration) async throws -> ConnectionInfo {
        let request = EmacsRequest(id: UUID().uuidString, operation: "ping")
        return try await execute(request, configuration: configuration, as: ConnectionInfo.self)
    }

    func listSessions(configuration: EmacsConfiguration) async throws -> SessionList {
        let request = EmacsRequest(id: UUID().uuidString, operation: "list-sessions")
        return try await execute(request, configuration: configuration, as: SessionList.self)
    }

    func selectSession(configuration: EmacsConfiguration, sessionID: String) async throws -> SelectedSession {
        let request = EmacsRequest(
            id: UUID().uuidString,
            operation: "select-session",
            params: ["session_id": sessionID]
        )
        return try await execute(request, configuration: configuration, as: SelectedSession.self)
    }

    func sendPrompt(
        configuration: EmacsConfiguration,
        sessionID: String,
        prompt: String
    ) async throws -> AcceptedPrompt {
        let request = EmacsRequest(
            id: UUID().uuidString,
            operation: "send-prompt",
            params: ["session_id": sessionID, "prompt": prompt]
        )
        return try await execute(request, configuration: configuration, as: AcceptedPrompt.self)
    }

    private func execute<Result: Decodable & Sendable>(
        _ request: EmacsRequest,
        configuration: EmacsConfiguration,
        as resultType: Result.Type
    ) async throws -> Result {
        let expression = try EmacsProtocolCodec.expression(for: request)

        // Process waiting and pipe reads run off the main actor.
        let output = try await Task.detached(priority: .utility) {
            guard FileManager.default.isExecutableFile(atPath: configuration.executablePath) else {
                throw EmacsClientError.executableNotFound
            }
            let process = Process()
            process.executableURL = URL(fileURLWithPath: configuration.executablePath)
            var arguments = ["--alternate-editor=false", "--timeout=5"]
            let socket = configuration.socketName.trimmingCharacters(in: .whitespacesAndNewlines)
            if !socket.isEmpty {
                arguments += ["--socket-name", socket]
            }
            process.arguments = arguments + ["--eval", expression]
            let stdout = Pipe()
            let stderr = Pipe()
            process.standardOutput = stdout
            process.standardError = stderr
            let start = ContinuousClock.now
            do {
                try process.run()
            } catch {
                throw EmacsClientError.processFailed(error.localizedDescription)
            }
            let timeout = DispatchWorkItem {
                if process.isRunning { process.terminate() }
            }
            DispatchQueue.global().asyncAfter(deadline: .now() + 8, execute: timeout)
            defer { timeout.cancel() }

            // Drain both pipes concurrently so a full stderr cannot block stdout.
            let outputTask = Task.detached { stdout.fileHandleForReading.readDataToEndOfFile() }
            let errorTask = Task.detached { stderr.fileHandleForReading.readDataToEndOfFile() }
            process.waitUntilExit()
            let data = await outputTask.value
            let errors = await errorTask.value
            if start.duration(to: .now) >= .seconds(8) {
                throw EmacsClientError.timedOut
            }
            guard process.terminationStatus == 0 else {
                let details = String(data: errors, encoding: .utf8)?
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                throw EmacsClientError.processFailed(
                    details.flatMap { $0.isEmpty ? nil : $0 }
                        ?? "emacsclient 退出码 \(process.terminationStatus)"
                )
            }
            return data
        }.value

        return try EmacsProtocolCodec.decode(output, requestID: request.id, as: resultType)
    }
}
