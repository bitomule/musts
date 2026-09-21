import ComposableArchitecture
import Foundation
import BoxyCore

@Reducer
struct CreateBoxFeature {
    @ObservableState
    struct State: Equatable {
        enum Mode: Equatable {
            case creation
            case edition(BoxModel)
        }
        let categoryId: UUID
        let mode: Mode
        var identifier: String = ""
        var alias: String = ""
        var generatedIdentifier: String = ""
        var icon: String = Box.defaultIcon
        var manualDescription: String = ""
        var identifierError: Bool = false
        /// Set only after a save attempt. Checking on every keystroke would issue a fetch per
        /// character typed for a field that is four characters long.
        var identifierTaken: Bool = false
        /// The availability check itself failed. Distinct from `identifierTaken`, because "we don't
        /// know" must not be shown — or acted on — as "it's free".
        var codeCheckFailed: Bool = false
        var isGeneratingCode: Bool = false
        var isCheckingCode: Bool = false

        var isEditing: Bool {
            switch mode {
            case .creation:
                return false
            case .edition:
                return true
            }
        }

        var isFormValid: Bool {
            !identifier.isEmpty && identifier.count == 4
        }

        init(categoryId: UUID, mode: Mode) {
            self.categoryId = categoryId
            self.mode = mode
            switch mode {
            case .creation:
               break
            case .edition(let box):
                self.identifier = box.identifier
                self.alias = box.alias ?? ""
                self.icon = box.icon
                self.manualDescription = box.manualDescription ?? ""
            }
        }
    }

    enum Action: BindableAction {
        case binding(BindingAction<State>)
        case onAppear
        case generateNewCodeTapped
        case codeGenerated(String)
        case codeGeneratedError(Error)
        case cancelButtonTapped
        case saveTapped
        case codeAvailabilityChecked(isAvailable: Bool, identifier: String)
        case codeCheckFailed
        case saveResult(BoxModel)
        case saveError(Error)
    }

    @Dependency(\.generateRandomUniqueCodeUseCase) var generateCode
    @Dependency(\.checkBoxCodeAvailable) var checkBoxCodeAvailable
    @Dependency(\.createBoxUseCase) var createBox
    @Dependency(\.editBoxUseCase) var editBox
    @Dependency(\.dismiss) var dismiss
    @Dependency(\.trackingUseCase) var tracking
    @Dependency(\.getHasCreatedBoxUseCase) var getHasCreatedBox
    @Dependency(\.setHasCreatedBoxUseCase) var setHasCreatedBox

    var body: some ReducerOf<Self> {
        BindingReducer()
        Reduce { state, action in
            switch action {
            case .onAppear:
                guard !state.isEditing else {
                    return .none
                }
                state.isGeneratingCode = true
                return .run { send in
                    do {
                        await send(.codeGenerated(try await generateCode.execute()))
                    } catch {
                        await send(.codeGeneratedError(error))
                    }
                }
            case .generateNewCodeTapped:
                state.isGeneratingCode = true
                return .run { send in
                    do {
                        await send(.codeGenerated(try await generateCode.execute()))
                    } catch {
                        await send(.codeGeneratedError(error))
                    }
                }
            case .codeGenerated(let code):
                state.identifier = code
                state.generatedIdentifier = code
                state.isGeneratingCode = false
                state.identifierError = false
                state.identifierTaken = false
                return .none
            case .codeGeneratedError:
                state.isGeneratingCode = false
                return .none
            case .cancelButtonTapped:
                return .run { @MainActor _ in
                    await dismiss()
                }
            case .saveTapped:
                let formattedIdentifier = state.identifier.uppercased()
                guard isValidIdentifier(formattedIdentifier) else {
                    state.identifierError = true
                    return .none
                }
                // The generator avoids collisions, but the field is editable and nothing used to
                // check what the user typed over it. Codes are unique app-wide, so this is the
                // last gate before a duplicate is written.
                state.isCheckingCode = true
                state.codeCheckFailed = false
                let excludingBoxId: UUID?
                switch state.mode {
                case .creation:
                    excludingBoxId = nil
                case let .edition(originalBox):
                    // Saving a box without touching its code must not report the code as taken
                    // against itself.
                    excludingBoxId = originalBox.id
                }
                return .run { [excludingBoxId] send in
                    do {
                        let isAvailable = try await checkBoxCodeAvailable.execute(
                            code: formattedIdentifier,
                            excludingBoxId: excludingBoxId
                        )
                        await send(.codeAvailabilityChecked(isAvailable: isAvailable, identifier: formattedIdentifier))
                    } catch {
                        // "The check failed" is not "the code is free". Failing open here would
                        // write the duplicate this whole change exists to stop.
                        await send(.codeCheckFailed)
                    }
                }

            case .codeCheckFailed:
                state.isCheckingCode = false
                state.codeCheckFailed = true
                return .none

            case let .codeAvailabilityChecked(isAvailable, formattedIdentifier):
                state.isCheckingCode = false
                // The field stays editable while the check runs; a code that changed underneath was
                // never checked, so nothing is written for it.
                guard state.identifier.uppercased() == formattedIdentifier else { return .none }
                guard isAvailable else {
                    state.identifierTaken = true
                    return .none
                }
                state.identifierTaken = false
                switch state.mode {
                case .creation:
                    return .run { [identifier = formattedIdentifier, alias = state.alias, icon = state.icon, categoryId = state.categoryId, description = state.manualDescription] send in
                        do {
                            let box = try await createBox.execute(identifier: identifier, alias: alias.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty, icon: icon, categoryId: categoryId, manualDescription: description.isEmpty ? nil : description)
                            await send(.saveResult(box))
                        } catch {
                            await send(.saveError(error))
                        }
                    }
                case .edition(let originalBox):
                    return .run { [identifier = formattedIdentifier, alias = state.alias, icon = state.icon, description = state.manualDescription] send in
                        do {
                            let box = try await editBox.execute(id: originalBox.id, identifier: identifier, alias: alias.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty, icon: icon, manualDescription: description.isEmpty ? nil : description)
                            await send(.saveResult(box))
                        } catch {
                            await send(.saveError(error))
                        }
                    }
                }

            case .saveResult:
                switch state.mode {
                case .creation:
                    let isFirst = !getHasCreatedBox.execute()
                    if isFirst {
                        setHasCreatedBox.execute()
                    }
                    tracking.execute(.boxCreated(
                        isFirst: isFirst,
                        hasName: !state.alias.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                        keptGeneratedCode: !state.generatedIdentifier.isEmpty
                            && state.identifier.uppercased() == state.generatedIdentifier.uppercased()
                    ))
                case .edition:
                    break
                }
                return .none
            case .saveError:
                return .none
            case .binding(\.identifier):
                state.identifier = state.identifier.uppercased()
                state.identifierError = !isValidIdentifier(state.identifier)
                // Editing the code clears a stale "already used" verdict; the next save re-checks.
                state.identifierTaken = false
                state.codeCheckFailed = false
                return .none
            case .binding:
                return .none
            }
        }
    }

    private func isValidIdentifier(_ identifier: String) -> Bool {
        BoxCodeValidator.isValid(identifier)
    }
}
