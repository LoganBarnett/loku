module Page.Rips exposing (Model, Msg, init, subscriptions, update, view)

{-| Disc ripping and identification, both polled from the server; the
operator's edits in the identification forms survive a poll.
-}

import Api
    exposing
        ( ActiveRip
        , DiscKind(..)
        , Drive
        , DriveMedia
        , IdentityCandidate
        , IdentityState(..)
        , ResolvedIdentity
        , RipJob
        , RipPolicy(..)
        , RipState(..)
        , RipTrigger(..)
        , RipsOverview
        , TmdbKind(..)
        , UnresolvedDisc
        , UnresolvedDiscs
        )
import Dict exposing (Dict)
import Html exposing (..)
import Html.Attributes exposing (..)
import Html.Events exposing (onClick, onInput, onSubmit)
import Route
import Task
import Time
import Ui


type alias Model =
    { zone : Time.Zone
    , now : Maybe Time.Posix
    , rips : Remote RipsOverview
    , ripsFetching : Bool
    , unresolved : Remote UnresolvedDiscs
    , unresolvedFetching : Bool
    , identityForms : Dict DiscKey IdentityForm
    , pendingRip : Maybe String
    , pendingIdentity : Maybe DiscKey
    , lastError : Maybe String
    , lastResolved : Maybe ResolvedIdentity
    }


type Remote a
    = Loading
    | Loaded a
    | Failed String


{-| A disc is addressed by its library and directory path, which is what
the identity endpoints take.
-}
type alias DiscKey =
    ( String, String )


{-| The operator's re-search state for one disc. `candidates` stays
`Nothing` until a re-search returns, at which point it overrides the
server's own list.
-}
type alias IdentityForm =
    { query : String
    , year : String
    , candidates : Maybe (List IdentityCandidate)
    }


type Msg
    = GotZone Time.Zone
    | RipsTick Time.Posix
    | UnresolvedTick Time.Posix
    | GotRips (Result Api.Error RipsOverview)
    | GotUnresolved (Result Api.Error UnresolvedDiscs)
    | StartRip String RipPolicy
    | RipStarted String (Result Api.Error RipJob)
    | QueryInput DiscKey String
    | YearInput DiscKey String
    | SearchSubmit DiscKey
    | GotCandidates DiscKey (Result Api.Error (List IdentityCandidate))
    | PickCandidate DiscKey IdentityCandidate
    | IdentityConfirmed DiscKey (Result Api.Error ResolvedIdentity)
    | DismissError


{-| Polling interval while a rip is running, so progress reads live.
-}
activeIntervalMs : Float
activeIntervalMs =
    2000


{-| Polling interval when nothing is happening; disc insertions and worker
identity attempts are rare enough that a slow poll suffices.
-}
idleIntervalMs : Float
idleIntervalMs =
    15000


{-| `Time.every` fires only after a full interval, so both sections are
fetched up front.
-}
init : ( Model, Cmd Msg )
init =
    ( { zone = Time.utc
      , now = Nothing
      , rips = Loading
      , ripsFetching = True
      , unresolved = Loading
      , unresolvedFetching = True
      , identityForms = Dict.empty
      , pendingRip = Nothing
      , pendingIdentity = Nothing
      , lastError = Nothing
      , lastResolved = Nothing
      }
    , Cmd.batch
        [ Task.perform GotZone Time.here
        , Api.getRips GotRips
        , Api.getUnresolvedDiscs GotUnresolved
        ]
    )


subscriptions : Model -> Sub Msg
subscriptions model =
    Sub.batch
        [ Time.every (ripsIntervalMs model.rips) RipsTick
        , Time.every idleIntervalMs UnresolvedTick
        ]


ripsIntervalMs : Remote RipsOverview -> Float
ripsIntervalMs rips =
    case rips of
        Loaded overview ->
            if anyActive overview then
                activeIntervalMs

            else
                idleIntervalMs

        _ ->
            idleIntervalMs


anyActive : RipsOverview -> Bool
anyActive overview =
    List.any (\drive -> drive.active /= Nothing) overview.drives



-- UPDATE


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotZone zone ->
            ( { model | zone = zone }, Cmd.none )

        RipsTick now ->
            -- Overlapping polls would stack up behind a slow server, so a
            -- tick during a fetch only advances the clock.
            if model.ripsFetching then
                ( { model | now = Just now }, Cmd.none )

            else
                ( { model | now = Just now, ripsFetching = True }
                , Api.getRips GotRips
                )

        UnresolvedTick now ->
            if model.unresolvedFetching then
                ( { model | now = Just now }, Cmd.none )

            else
                ( { model | now = Just now, unresolvedFetching = True }
                , Api.getUnresolvedDiscs GotUnresolved
                )

        GotRips (Ok overview) ->
            ( { model | rips = Loaded overview, ripsFetching = False }
            , Cmd.none
            )

        GotRips (Err err) ->
            let
                ( rips, lastError ) =
                    failSection err model.rips model.lastError
            in
            ( { model | rips = rips, ripsFetching = False, lastError = lastError }
            , Cmd.none
            )

        GotUnresolved (Ok unresolved) ->
            ( { model
                | unresolved = Loaded unresolved
                , unresolvedFetching = False
                , identityForms = pruneForms unresolved.discs model.identityForms
              }
            , Cmd.none
            )

        GotUnresolved (Err err) ->
            let
                ( unresolved, lastError ) =
                    failSection err model.unresolved model.lastError
            in
            ( { model
                | unresolved = unresolved
                , unresolvedFetching = False
                , lastError = lastError
              }
            , Cmd.none
            )

        StartRip device policy ->
            ( { model | pendingRip = Just device, lastError = Nothing }
            , Api.postRip { device = device, policy = policy } (RipStarted device)
            )

        RipStarted device (Ok _) ->
            -- The next tick may be a full idle interval off, so refetch
            -- straight away to show the new job.
            ( { model
                | pendingRip = clearIf device model.pendingRip
                , ripsFetching = True
              }
            , Api.getRips GotRips
            )

        RipStarted device (Err err) ->
            ( { model
                | pendingRip = clearIf device model.pendingRip
                , lastError = Just (Api.errorToString err)
              }
            , Cmd.none
            )

        QueryInput key query ->
            ( updateForm key (\form -> { form | query = query }) model
            , Cmd.none
            )

        YearInput key year ->
            ( updateForm key (\form -> { form | year = year }) model
            , Cmd.none
            )

        SearchSubmit key ->
            let
                form =
                    formFor model key

                query =
                    String.trim form.query
            in
            if String.isEmpty query then
                ( model, Cmd.none )

            else
                ( { model | pendingIdentity = Just key, lastError = Nothing }
                , Api.postIdentify
                    { library = Tuple.first key
                    , path = Tuple.second key
                    , query = query
                    , year = String.toInt (String.trim form.year)
                    }
                    (GotCandidates key)
                )

        GotCandidates key (Ok candidates) ->
            ( updateForm key
                (\form -> { form | candidates = Just candidates })
                { model | pendingIdentity = clearIf key model.pendingIdentity }
            , Cmd.none
            )

        GotCandidates key (Err err) ->
            ( { model
                | pendingIdentity = clearIf key model.pendingIdentity
                , lastError = Just (Api.errorToString err)
              }
            , Cmd.none
            )

        PickCandidate key candidate ->
            ( { model | pendingIdentity = Just key, lastError = Nothing }
            , Api.postIdentity
                { library = Tuple.first key
                , path = Tuple.second key
                , tmdbKind = candidate.tmdbKind
                , tmdbId = candidate.tmdbId
                }
                (IdentityConfirmed key)
            )

        IdentityConfirmed key (Ok resolved) ->
            -- Drop the disc optimistically; the refetch of both sections
            -- picks up the renamed directory and the disc card's new title.
            ( { model
                | pendingIdentity = clearIf key model.pendingIdentity
                , identityForms = Dict.remove key model.identityForms
                , lastResolved = Just resolved
                , unresolved =
                    mapLoaded
                        (\data ->
                            { data
                                | discs =
                                    List.filter
                                        (\disc -> discKey disc /= key)
                                        data.discs
                            }
                        )
                        model.unresolved
                , ripsFetching = True
                , unresolvedFetching = True
              }
            , Cmd.batch
                [ Api.getRips GotRips
                , Api.getUnresolvedDiscs GotUnresolved
                ]
            )

        IdentityConfirmed key (Err err) ->
            ( { model
                | pendingIdentity = clearIf key model.pendingIdentity
                , lastError = Just (Api.errorToString err)
              }
            , Cmd.none
            )

        DismissError ->
            ( { model | lastError = Nothing }, Cmd.none )


{-| Apply a failed poll to one section. A poll blip must not blank a loaded
page, so a section that already holds data keeps it and the failure goes on
the page's error line; a section still loading shows the failure in place.
-}
failSection : Api.Error -> Remote a -> Maybe String -> ( Remote a, Maybe String )
failSection err section lastError =
    case section of
        Loaded _ ->
            ( section, Just (Api.errorToString err) )

        _ ->
            ( Failed (Api.errorToString err), lastError )


mapLoaded : (a -> a) -> Remote a -> Remote a
mapLoaded transform remote =
    case remote of
        Loaded data ->
            Loaded (transform data)

        _ ->
            remote


{-| A stale response could re-enable another disc's buttons, so a pending
marker is cleared only when it belongs to the request that just completed.
-}
clearIf : a -> Maybe a -> Maybe a
clearIf expected pending =
    if pending == Just expected then
        Nothing

    else
        pending


discKey : UnresolvedDisc -> DiscKey
discKey disc =
    ( disc.library, disc.path )


{-| The dictionary would grow without bound across a long-lived page, so
forms for discs the server no longer lists are dropped.
-}
pruneForms : List UnresolvedDisc -> Dict DiscKey IdentityForm -> Dict DiscKey IdentityForm
pruneForms discs forms =
    Dict.filter (\key _ -> List.any (\disc -> discKey disc == key) discs) forms


updateForm : DiscKey -> (IdentityForm -> IdentityForm) -> Model -> Model
updateForm key transform model =
    { model
        | identityForms =
            Dict.insert key (transform (formFor model key)) model.identityForms
    }


{-| The re-search form for a disc: the operator's edits when there are
any, else a fresh form seeded from the server's own query.
-}
formFor : Model -> DiscKey -> IdentityForm
formFor model key =
    Dict.get key model.identityForms
        |> Maybe.withDefault (defaultForm (discFor model key))


defaultForm : Maybe UnresolvedDisc -> IdentityForm
defaultForm disc =
    { query = disc |> Maybe.map .query |> Maybe.withDefault ""
    , year = ""
    , candidates = Nothing
    }


discFor : Model -> DiscKey -> Maybe UnresolvedDisc
discFor model key =
    case model.unresolved of
        Loaded { discs } ->
            discs |> List.filter (\disc -> discKey disc == key) |> List.head

        _ ->
            Nothing



-- VIEW


view : Model -> Html Msg
view model =
    div []
        [ nav [ class "crumb-bar" ]
            [ a [ href (Route.toString Route.Home) ] [ text "Home" ]
            , text " / Rips"
            ]
        , div [ class "page" ]
            [ h1 [] [ text "Rips" ]
            , viewMaybe viewError model.lastError
            , viewMaybe viewResolved model.lastResolved
            , viewBanners model
            , viewDrives model
            , viewIdentification model
            , viewHistory model
            ]
        ]


viewError : String -> Html Msg
viewError message =
    p [ class "error-note" ]
        [ text ("Error: " ++ message)
        , button [ onClick DismissError, class "error-dismiss" ] [ text "Dismiss" ]
        ]


viewResolved : ResolvedIdentity -> Html Msg
viewResolved resolved =
    p [ class "success-note" ]
        [ text ("Identified: " ++ resolved.title ++ yearSuffix resolved.year ++ " — now at ")
        , a
            [ href
                (Route.toString
                    (Route.Browse
                        { library = resolved.library
                        , path = parentPath resolved.path
                        , page = 1
                        }
                    )
                )
            ]
            [ text (resolved.library ++ "/" ++ resolved.path) ]
        , text
            (if List.isEmpty resolved.artworkWarnings then
                ""

             else
                " — artwork: " ++ String.join "; " resolved.artworkWarnings
            )
        ]


viewBanners : Model -> Html Msg
viewBanners model =
    div []
        [ case model.rips of
            Loaded { enabled } ->
                if enabled then
                    text ""

                else
                    p [ class "empty-note" ]
                        [ text "Ripping is not configured on this server" ]

            _ ->
                text ""
        , case model.unresolved of
            Loaded { identityEnabled } ->
                if identityEnabled then
                    text ""

                else
                    p [ class "empty-note" ]
                        [ text "Identity resolution is disabled (no TMDB API key)" ]

            _ ->
                text ""
        ]



-- DRIVES


viewDrives : Model -> Html Msg
viewDrives model =
    section [ class "rips-section" ]
        (h2 [] [ text "Drives" ]
            :: viewSection model.rips
                (\overview ->
                    if List.isEmpty overview.drives then
                        [ p [ class "empty-note" ] [ text "No drives configured" ] ]

                    else
                        List.map (viewDrive model) overview.drives
                )
        )


{-| The body of a polled section: a loading line, the fetch error, or the
loaded rendering.
-}
viewSection : Remote a -> (a -> List (Html Msg)) -> List (Html Msg)
viewSection remote render =
    case remote of
        Loading ->
            [ p [] [ text "Loading…" ] ]

        Failed err ->
            [ Ui.errorText err ]

        Loaded data ->
            render data


viewDrive : Model -> Drive -> Html Msg
viewDrive model drive =
    article [ class "drive-card" ]
        ([ div [ class "drive-device" ] [ text drive.device ]
         , div [ class "drive-meta" ]
            [ text (drive.library ++ " · " ++ policyLabel drive.policy) ]
         , viewMaybe (\err -> p [ class "error-text" ] [ text err ]) drive.lastError
         ]
            ++ (case ( drive.active, drive.media ) of
                    ( Just active, _ ) ->
                        viewActive model.now active

                    ( Nothing, Just media ) ->
                        viewIdleMedia model.pendingRip drive.device media

                    ( Nothing, Nothing ) ->
                        [ p [ class "drive-meta" ] [ text "No disc in drive" ] ]
               )
        )


viewActive : Maybe Time.Posix -> ActiveRip -> List (Html Msg)
viewActive now active =
    [ p []
        [ ripStateBadge active.state
        , text (active.label |> Maybe.map (\label -> " " ++ label) |> Maybe.withDefault "")
        ]
    , progress
        [ class "rip-progress"
        , attribute "max" "1000"
        , attribute "value" (String.fromInt active.progressPermille)
        ]
        []
    , p [ class "rip-op" ]
        [ text
            (String.fromInt (active.progressPermille // 10)
                ++ "%"
                ++ joinPresent " · " [ active.currentOp, active.totalOp ]
            )
        ]
    , viewMaybe
        (\posix ->
            p [ class "drive-meta" ]
                [ text
                    ("started "
                        ++ formatElapsed (Time.posixToMillis posix // 1000 - active.startedAt)
                        ++ " ago"
                    )
                ]
        )
        now
    ]


viewIdleMedia : Maybe String -> String -> DriveMedia -> List (Html Msg)
viewIdleMedia pendingRip device media =
    let
        pending =
            pendingRip == Just device

        ripButton policy label =
            button
                [ onClick (StartRip device policy)
                , disabled pending
                , class "drive-button"
                ]
                [ text label ]
    in
    [ p []
        [ text
            (discKindLabel media.kind
                ++ (media.label |> Maybe.map (\label -> " · " ++ label) |> Maybe.withDefault "")
            )
        ]
    , viewMaybe
        (\discId -> p [ class "drive-meta" ] [ text ("disc id " ++ discId) ])
        media.discId
    , p [ class "drive-meta" ]
        [ text
            (media.existingPath
                |> Maybe.map (\path -> "already ripped to " ++ path)
                |> Maybe.withDefault "not yet ripped"
            )
        ]
    , div [ class "drive-actions" ]
        (List.filterMap identity
            [ if media.existingPath == Nothing then
                Just (ripButton Skip "Rip now")

              else
                Nothing
            , Just (ripButton Rescan "Rescan")
            , Just (ripButton Rerip "Rerip")
            ]
        )
    ]



-- IDENTIFICATION


viewIdentification : Model -> Html Msg
viewIdentification model =
    section [ class "rips-section" ]
        (h2 [] [ text "Needs identification" ]
            :: viewSection model.unresolved
                (\{ discs } ->
                    if List.isEmpty discs then
                        [ p [ class "empty-note" ] [ text "Nothing needs identification" ] ]

                    else
                        List.map (viewDisc model) discs
                )
        )


viewDisc : Model -> UnresolvedDisc -> Html Msg
viewDisc model disc =
    let
        key =
            discKey disc

        form =
            Dict.get key model.identityForms
                |> Maybe.withDefault (defaultForm (Just disc))

        pending =
            model.pendingIdentity == Just key
    in
    article [ class "identity-disc" ]
        [ div [ class "identity-label" ] [ text disc.label ]
        , div [ class "drive-meta" ] [ text (disc.library ++ " / " ++ disc.path) ]
        , p []
            [ identityStateBadge disc.identityState
            , text (" " ++ discHints model.zone disc)
            ]
        , viewMaybe (\err -> p [ class "error-text" ] [ text err ]) disc.error
        , Html.form
            [ onSubmit (SearchSubmit key), class "search-form identity-form" ]
            [ input
                [ type_ "search"
                , placeholder "Title"
                , value form.query
                , onInput (QueryInput key)
                , class "search-input"
                ]
                []
            , input
                [ type_ "number"
                , placeholder "Year"
                , value form.year
                , onInput (YearInput key)
                , class "search-input year-input"
                ]
                []
            , button
                [ type_ "submit", disabled pending, class "search-button" ]
                [ text "Search" ]
            ]
        , viewCandidates key pending (Maybe.withDefault disc.candidates form.candidates)
        ]


{-| What the server knew when it searched, so the operator can see why a
candidate list looks the way it does.
-}
discHints : Time.Zone -> UnresolvedDisc -> String
discHints zone disc =
    joinPresent " · "
        [ disc.yearHint |> Maybe.map (\year -> "year hint " ++ String.fromInt year)
        , disc.kindHint |> Maybe.map (\kind -> "looks like " ++ tmdbKindLabel kind)
        , if disc.fingerprintStrong then
            Just "strong fingerprint"

          else
            Nothing
        , disc.attemptedAt |> Maybe.map (\at -> "last attempt " ++ formatTime zone at)
        ]
        |> String.dropLeft (String.length " · ")


viewCandidates : DiscKey -> Bool -> List IdentityCandidate -> Html Msg
viewCandidates key pending candidates =
    if List.isEmpty candidates then
        p [ class "empty-note" ] [ text "No candidates; try another query." ]

    else
        div [ class "candidate-list" ]
            (List.map (viewCandidate key pending) candidates)


viewCandidate : DiscKey -> Bool -> IdentityCandidate -> Html Msg
viewCandidate key pending candidate =
    article [ class "candidate-card" ]
        [ candidatePoster candidate
        , div [ class "candidate-title" ]
            [ text (candidate.title ++ yearSuffix candidate.year) ]
        , div [ class "candidate-kind" ]
            [ text
                (tmdbKindLabel candidate.tmdbKind
                    ++ " · "
                    ++ String.fromInt candidate.voteCount
                    ++ " votes"
                    ++ (candidate.originalTitle
                            |> Maybe.map (\original -> " · " ++ original)
                            |> Maybe.withDefault ""
                       )
                )
            ]
        , viewMaybe
            (\overview -> p [ class "candidate-overview" ] [ text overview ])
            candidate.overview
        , button
            [ onClick (PickCandidate key candidate)
            , disabled pending
            , class "candidate-pick"
            ]
            [ text "This one" ]
        ]


candidatePoster : IdentityCandidate -> Html Msg
candidatePoster candidate =
    candidate.posterUrl
        |> Maybe.map
            (\url ->
                img
                    [ src url
                    , alt candidate.title
                    , attribute "loading" "lazy"
                    , class "candidate-poster"
                    ]
                    []
            )
        |> Maybe.withDefault
            (div [ class "candidate-poster-placeholder" ] [ text "No poster" ])



-- HISTORY


viewHistory : Model -> Html Msg
viewHistory model =
    section [ class "rips-section" ]
        (h2 [] [ text "Recent rips" ]
            :: viewSection model.rips
                (\{ recent } ->
                    if List.isEmpty recent then
                        [ p [ class "empty-note" ] [ text "No rips yet" ] ]

                    else
                        [ table [ class "rip-table" ]
                            [ thead []
                                [ tr []
                                    (List.map (\heading -> th [] [ text heading ])
                                        [ "State", "Disc", "Device", "Started", "Finished", "Error" ]
                                    )
                                ]
                            , tbody [] (List.map (viewJob model.zone) recent)
                            ]
                        ]
                )
        )


viewJob : Time.Zone -> RipJob -> Html Msg
viewJob zone job =
    tr []
        [ td [] [ ripStateBadge job.state ]
        , td []
            [ text (Maybe.withDefault "—" job.label)
            , div [ class "drive-meta" ] [ text (Maybe.withDefault "" job.path) ]
            ]
        , td []
            [ text job.device
            , div [ class "drive-meta" ]
                [ text (policyLabel job.policy ++ " · " ++ triggerLabel job.trigger) ]
            ]
        , td [] [ text (formatTime zone job.startedAt) ]
        , td []
            [ text
                (job.finishedAt
                    |> Maybe.map (formatTime zone)
                    |> Maybe.withDefault "—"
                )
            ]
        , td [ class "rip-error" ] [ text (Maybe.withDefault "" job.error) ]
        ]



-- BADGES AND LABELS


stateBadge : String -> String -> Html Msg
stateBadge modifier label =
    span [ class ("state-badge " ++ modifier) ] [ text label ]


ripStateBadge : RipState -> Html Msg
ripStateBadge state =
    stateBadge (ripStateModifier state) (ripStateLabel state)


ripStateModifier : RipState -> String
ripStateModifier state =
    case state of
        RipScanning ->
            "state-badge-active"

        RipRipping ->
            "state-badge-active"

        RipFinalizing ->
            "state-badge-active"

        RipDone ->
            "state-badge-done"

        RipFailed ->
            "state-badge-failed"

        RipDetected ->
            "state-badge-idle"

        RipSkipped ->
            "state-badge-idle"

        RipUnknown _ ->
            "state-badge-idle"


ripStateLabel : RipState -> String
ripStateLabel state =
    case state of
        RipDetected ->
            "detected"

        RipScanning ->
            "scanning"

        RipRipping ->
            "ripping"

        RipFinalizing ->
            "finalizing"

        RipDone ->
            "done"

        RipSkipped ->
            "skipped"

        RipFailed ->
            "failed"

        RipUnknown other ->
            humanize other


identityStateBadge : IdentityState -> Html Msg
identityStateBadge state =
    stateBadge (identityStateModifier state) (identityStateLabel state)


identityStateModifier : IdentityState -> String
identityStateModifier state =
    case state of
        IdentityUnresolved ->
            "state-badge-warn"

        IdentityNeedsConfirmation ->
            "state-badge-warn"

        IdentityResolved ->
            "state-badge-done"

        IdentityFailed ->
            "state-badge-failed"

        IdentityUnknown _ ->
            "state-badge-idle"


identityStateLabel : IdentityState -> String
identityStateLabel state =
    case state of
        IdentityUnresolved ->
            "unresolved"

        IdentityNeedsConfirmation ->
            "needs confirmation"

        IdentityResolved ->
            "resolved"

        IdentityFailed ->
            "failed"

        IdentityUnknown other ->
            humanize other


policyLabel : RipPolicy -> String
policyLabel policy =
    case policy of
        Skip ->
            "skip"

        Rescan ->
            "rescan"

        Rerip ->
            "rerip"


triggerLabel : RipTrigger -> String
triggerLabel trigger =
    case trigger of
        Auto ->
            "auto"

        Manual ->
            "manual"

        TriggerUnknown other ->
            humanize other


discKindLabel : DiscKind -> String
discKindLabel kind =
    case kind of
        Dvd ->
            "DVD"

        Bd ->
            "Blu-ray"

        DiscKindUnknown other ->
            humanize other


tmdbKindLabel : TmdbKind -> String
tmdbKindLabel kind =
    case kind of
        Movie ->
            "movie"

        Tv ->
            "TV"


{-| Server enum values the page does not know yet are shown as-is, with
the snake case softened.
-}
humanize : String -> String
humanize =
    String.replace "_" " "



-- HELPERS


viewMaybe : (a -> Html msg) -> Maybe a -> Html msg
viewMaybe render maybe =
    maybe |> Maybe.map render |> Maybe.withDefault (text "")


yearSuffix : Maybe Int -> String
yearSuffix year =
    year
        |> Maybe.map (\y -> " (" ++ String.fromInt y ++ ")")
        |> Maybe.withDefault ""


{-| Join the present values, each preceded by the separator, so the result
appends cleanly to a leading piece of text.
-}
joinPresent : String -> List (Maybe String) -> String
joinPresent separator parts =
    parts
        |> List.filterMap identity
        |> List.map (\part -> separator ++ part)
        |> String.concat


parentPath : String -> String
parentPath path =
    path
        |> String.split "/"
        |> List.reverse
        |> List.drop 1
        |> List.reverse
        |> String.join "/"


{-| Unix seconds as a local `YYYY-MM-DD HH:MM`.
-}
formatTime : Time.Zone -> Int -> String
formatTime zone secs =
    let
        posix =
            Time.millisToPosix (secs * 1000)

        pad2 n =
            String.padLeft 2 '0' (String.fromInt n)
    in
    String.fromInt (Time.toYear zone posix)
        ++ "-"
        ++ pad2 (monthNumber (Time.toMonth zone posix))
        ++ "-"
        ++ pad2 (Time.toDay zone posix)
        ++ " "
        ++ pad2 (Time.toHour zone posix)
        ++ ":"
        ++ pad2 (Time.toMinute zone posix)


monthNumber : Time.Month -> Int
monthNumber month =
    case month of
        Time.Jan ->
            1

        Time.Feb ->
            2

        Time.Mar ->
            3

        Time.Apr ->
            4

        Time.May ->
            5

        Time.Jun ->
            6

        Time.Jul ->
            7

        Time.Aug ->
            8

        Time.Sep ->
            9

        Time.Oct ->
            10

        Time.Nov ->
            11

        Time.Dec ->
            12


{-| A duration in seconds as `3m 12s`, with hours in front once it gets
that long. A clock that runs behind the server clamps to zero.
-}
formatElapsed : Int -> String
formatElapsed secs =
    let
        total =
            Basics.max 0 secs

        hours =
            total // 3600

        minutes =
            modBy 60 (total // 60)

        seconds =
            modBy 60 total

        minutesAndSeconds =
            String.fromInt minutes ++ "m " ++ String.fromInt seconds ++ "s"
    in
    if hours > 0 then
        String.fromInt hours ++ "h " ++ minutesAndSeconds

    else
        minutesAndSeconds
